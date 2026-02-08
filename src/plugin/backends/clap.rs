use std::path::Path;

use crate::plugin::protocol::{
    GuiCapabilities, PluginDescriptorInfo, PluginParamInfo, PluginParamValue,
};

#[cfg(feature = "plugin_native_clap")]
mod native {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::{CString, OsStr};
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    use base64::Engine;
    use clack_extensions::gui::{GuiApiType, GuiConfiguration, HostGui, HostGuiImpl, PluginGui};
    use clack_extensions::params::{
        HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamInfoBuffer, PluginParams,
    };
    use clack_extensions::state::{HostState, HostStateImpl, PluginState};
    use clack_host::events::event_types::ParamValueEvent;
    use clack_host::prelude::*;
    use clack_host::utils::Cookie;

    use crate::plugin::backends::plugin_display_name;
    use crate::plugin::PluginFormat;

    #[derive(Default)]
    struct ClapHostShared {
        callback_requested: AtomicBool,
        gui_closed: AtomicBool,
        flush_requested: AtomicBool,
    }

    impl SharedHandler<'_> for ClapHostShared {
        fn request_restart(&self) {}
        fn request_process(&self) {}
        fn request_callback(&self) {
            self.callback_requested.store(true, Ordering::Release);
        }
    }

    impl HostGuiImpl for ClapHostShared {
        fn resize_hints_changed(&self) {}

        fn request_resize(
            &self,
            _new_size: clack_extensions::gui::GuiSize,
        ) -> Result<(), HostError> {
            Ok(())
        }

        fn request_show(&self) -> Result<(), HostError> {
            Ok(())
        }

        fn request_hide(&self) -> Result<(), HostError> {
            Ok(())
        }

        fn closed(&self, _was_destroyed: bool) {
            self.gui_closed.store(true, Ordering::Release);
        }
    }

    impl HostParamsImplShared for ClapHostShared {
        fn request_flush(&self) {
            self.flush_requested.store(true, Ordering::Release);
        }
    }

    #[derive(Default)]
    struct ClapHostMainThread {
        state_dirty: bool,
    }

    impl MainThreadHandler<'_> for ClapHostMainThread {}

    impl HostParamsImplMainThread for ClapHostMainThread {
        fn rescan(&mut self, _flags: clack_extensions::params::ParamRescanFlags) {}
        fn clear(&mut self, _param_id: ClapId, _flags: clack_extensions::params::ParamClearFlags) {}
    }

    impl HostStateImpl for ClapHostMainThread {
        fn mark_dirty(&mut self) {
            self.state_dirty = true;
        }
    }

    struct ClapHostHandlers;

    impl HostHandlers for ClapHostHandlers {
        type Shared<'a> = ClapHostShared;
        type MainThread<'a> = ClapHostMainThread;
        type AudioProcessor<'a> = ();

        fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
            builder
                .register::<HostGui>()
                .register::<HostParams>()
                .register::<HostState>();
        }
    }

    #[derive(Clone)]
    struct ClapParamSpec {
        id: String,
        clap_id: ClapId,
        min_plain: f64,
        max_plain: f64,
        default_plain: f64,
    }

    fn candidate_bundles(search_paths: &[String]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for raw in search_paths {
            let root = Path::new(raw);
            if !root.exists() {
                continue;
            }
            if root.is_file()
                && root
                    .extension()
                    .and_then(OsStr::to_str)
                    .map(|ext| ext.eq_ignore_ascii_case("clap"))
                    .unwrap_or(false)
            {
                out.push(root.to_path_buf());
                continue;
            }
            for entry in walkdir::WalkDir::new(root)
                .follow_links(false)
                .max_depth(8)
                .into_iter()
                .filter_map(Result::ok)
            {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                if p.extension()
                    .and_then(OsStr::to_str)
                    .map(|ext| ext.eq_ignore_ascii_case("clap"))
                    .unwrap_or(false)
                {
                    out.push(p.to_path_buf());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn descriptor_features(desc: &clack_host::plugin::PluginDescriptor) -> Vec<String> {
        desc.features()
            .map(|f| f.to_string_lossy().to_ascii_lowercase())
            .collect()
    }

    pub(super) fn has_synth_like_feature(features: &[String]) -> bool {
        features.iter().any(|f| {
            matches!(
                f.as_str(),
                "instrument" | "synthesizer" | "sampler" | "drum" | "drum-machine"
            )
        })
    }

    fn is_effect_features(features: &[String]) -> bool {
        if has_synth_like_feature(features) {
            return false;
        }
        if features.is_empty() {
            return true;
        }
        features.iter().any(|f| f == "audio-effect")
    }

    fn descriptor_name(desc: &clack_host::plugin::PluginDescriptor, plugin_path: &Path) -> String {
        desc.name()
            .map(|v| v.to_string_lossy().to_string())
            .unwrap_or_else(|| plugin_display_name(plugin_path))
    }

    fn select_descriptor(
        bundle: &PluginBundle,
        plugin_path: &Path,
    ) -> Result<(CString, String, Vec<String>), String> {
        let factory = bundle
            .get_plugin_factory()
            .ok_or_else(|| "clap missing plugin factory".to_string())?;

        let mut fallback: Option<(CString, String, Vec<String>)> = None;
        for desc in factory.plugin_descriptors() {
            let Some(id) = desc.id() else {
                continue;
            };
            let Ok(id_owned) = CString::new(id.to_bytes()) else {
                continue;
            };
            let name = descriptor_name(&desc, plugin_path);
            let features = descriptor_features(&desc);
            if fallback.is_none() {
                fallback = Some((id_owned.clone(), name.clone(), features.clone()));
            }
            if is_effect_features(&features) {
                return Ok((id_owned, name, features));
            }
        }

        fallback.ok_or_else(|| "clap has no plugin descriptor".to_string())
    }

    fn host_info() -> Result<HostInfo, String> {
        HostInfo::new("NeoWaves", "NeoWaves", "https://example.invalid", "0.1.0")
            .map_err(|e| format!("host info failed: {e}"))
    }

    fn instantiate_plugin(
        plugin_path: &Path,
    ) -> Result<
        (
            PluginBundle,
            PluginInstance<ClapHostHandlers>,
            String,
            Vec<String>,
        ),
        String,
    > {
        let bundle = unsafe { PluginBundle::load(plugin_path) }
            .map_err(|e| format!("clap load failed: {e}"))?;
        let (plugin_id, plugin_name, features) = select_descriptor(&bundle, plugin_path)?;
        let host_info = host_info()?;

        let instance = PluginInstance::<ClapHostHandlers>::new(
            |_| ClapHostShared::default(),
            |_| ClapHostMainThread::default(),
            &bundle,
            plugin_id.as_c_str(),
            &host_info,
        )
        .map_err(|e| format!("clap instantiate failed: {e}"))?;

        Ok((bundle, instance, plugin_name, features))
    }
    fn plain_to_normalized(value: f64, min_plain: f64, max_plain: f64) -> f32 {
        if !(value.is_finite() && min_plain.is_finite() && max_plain.is_finite()) {
            return 0.0;
        }
        if max_plain <= min_plain {
            return 0.0;
        }
        ((value - min_plain) / (max_plain - min_plain)).clamp(0.0, 1.0) as f32
    }

    fn normalized_to_plain(value: f32, min_plain: f64, max_plain: f64) -> f64 {
        if !(min_plain.is_finite() && max_plain.is_finite()) || max_plain <= min_plain {
            return value.clamp(0.0, 1.0) as f64;
        }
        min_plain + (max_plain - min_plain) * (value.clamp(0.0, 1.0) as f64)
    }

    fn parse_param_id(id: &str) -> Option<ClapId> {
        if let Some(raw) = id.strip_prefix("clap:") {
            return raw.trim().parse::<u32>().ok().and_then(ClapId::from_raw);
        }
        id.trim().parse::<u32>().ok().and_then(ClapId::from_raw)
    }

    fn collect_params(
        instance: &mut PluginInstance<ClapHostHandlers>,
    ) -> (
        Vec<PluginParamInfo>,
        Vec<ClapParamSpec>,
        Option<PluginParams>,
    ) {
        let mut handle = instance.plugin_handle();
        let Some(params_ext) = handle.get_extension::<PluginParams>() else {
            return (Vec::new(), Vec::new(), None);
        };

        let mut ui_params = Vec::new();
        let mut specs = Vec::new();

        let count = params_ext.count(&mut handle);
        for index in 0..count {
            let mut buffer = ParamInfoBuffer::new();
            let Some(info) = params_ext.get_info(&mut handle, index, &mut buffer) else {
                continue;
            };
            let min_plain = info.min_value;
            let max_plain = if info.max_value > info.min_value {
                info.max_value
            } else {
                info.min_value + 1.0
            };
            let default_plain = info.default_value.clamp(min_plain, max_plain);
            let current_plain = params_ext
                .get_value(&mut handle, info.id)
                .unwrap_or(default_plain)
                .clamp(min_plain, max_plain);
            let id_text = format!("clap:{}", info.id.get());
            let name = if info.name.is_empty() {
                format!("Param {}", info.id.get())
            } else {
                String::from_utf8_lossy(info.name).to_string()
            };
            let module = String::from_utf8_lossy(info.module).to_string();
            ui_params.push(PluginParamInfo {
                id: id_text.clone(),
                name,
                normalized: plain_to_normalized(current_plain, min_plain, max_plain),
                default_normalized: plain_to_normalized(default_plain, min_plain, max_plain),
                min: 0.0,
                max: 1.0,
                unit: module,
            });
            specs.push(ClapParamSpec {
                id: id_text,
                clap_id: info.id,
                min_plain,
                max_plain,
                default_plain,
            });
        }

        ui_params.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        specs.sort_by(|a, b| a.id.cmp(&b.id));

        (ui_params, specs, Some(params_ext))
    }

    fn apply_param_values_inactive(
        instance: &mut PluginInstance<ClapHostHandlers>,
        params_ext: PluginParams,
        specs: &[ClapParamSpec],
        params: &[PluginParamValue],
    ) {
        if params.is_empty() {
            return;
        }
        let Some(mut inactive) = instance.inactive_plugin_handle() else {
            return;
        };
        let by_id: HashMap<&str, &ClapParamSpec> =
            specs.iter().map(|s| (s.id.as_str(), s)).collect();

        let mut input_events = EventBuffer::with_capacity(params.len());
        let mut output_events = EventBuffer::new();

        for p in params {
            if let Some(spec) = by_id.get(p.id.as_str()) {
                let plain = normalized_to_plain(p.normalized, spec.min_plain, spec.max_plain);
                input_events.push(&ParamValueEvent::new(
                    0,
                    spec.clap_id,
                    Pckn::match_all(),
                    plain,
                    Cookie::empty(),
                ));
                continue;
            }
            if let Some(clap_id) = parse_param_id(&p.id) {
                input_events.push(&ParamValueEvent::new(
                    0,
                    clap_id,
                    Pckn::match_all(),
                    p.normalized.clamp(0.0, 1.0) as f64,
                    Cookie::empty(),
                ));
            }
        }

        params_ext.flush(
            &mut inactive,
            &input_events.as_input(),
            &mut output_events.as_output(),
        );
    }

    fn read_snapshot(
        instance: &mut PluginInstance<ClapHostHandlers>,
        params_ext: Option<PluginParams>,
        specs: &[ClapParamSpec],
    ) -> Vec<PluginParamValue> {
        let Some(params_ext) = params_ext else {
            return Vec::new();
        };
        if specs.is_empty() {
            return Vec::new();
        }

        let mut handle = instance.plugin_handle();
        let mut out = Vec::with_capacity(specs.len());
        for spec in specs {
            let plain = params_ext
                .get_value(&mut handle, spec.clap_id)
                .unwrap_or(spec.default_plain)
                .clamp(spec.min_plain, spec.max_plain);
            out.push(PluginParamValue {
                id: spec.id.clone(),
                normalized: plain_to_normalized(plain, spec.min_plain, spec.max_plain),
            });
        }
        out
    }

    fn decode_state_blob(state_blob_b64: &str) -> Result<Vec<u8>, String> {
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(state_blob_b64.as_bytes())
            .map_err(|e| format!("state decode failed: {e}"))
    }

    fn encode_state_blob(bytes: &[u8]) -> Option<String> {
        if bytes.is_empty() {
            return None;
        }
        Some(base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes))
    }

    fn maybe_load_state(
        instance: &mut PluginInstance<ClapHostHandlers>,
        state_ext: Option<PluginState>,
        state_blob_b64: Option<&str>,
    ) -> Result<(), String> {
        let Some(state_ext) = state_ext else {
            return Ok(());
        };
        let Some(raw) = state_blob_b64 else {
            return Ok(());
        };
        let bytes = decode_state_blob(raw)?;
        let mut handle = instance.plugin_handle();
        state_ext
            .load(&mut handle, &mut Cursor::new(bytes))
            .map_err(|e| format!("state load failed: {e}"))
    }

    fn maybe_save_state(
        instance: &mut PluginInstance<ClapHostHandlers>,
        state_ext: Option<PluginState>,
    ) -> Option<String> {
        let state_ext = state_ext?;
        let mut bytes = Vec::new();
        let mut handle = instance.plugin_handle();
        if state_ext.save(&mut handle, &mut bytes).is_ok() {
            encode_state_blob(&bytes)
        } else {
            None
        }
    }

    fn negotiate_gui_configuration(
        gui_ext: &PluginGui,
        handle: &mut PluginMainThreadHandle<'_>,
    ) -> Option<GuiConfiguration<'static>> {
        let api_type = GuiApiType::default_for_current_platform()?;

        let embedded = GuiConfiguration {
            api_type,
            is_floating: false,
        };
        if gui_ext.is_api_supported(handle, embedded) {
            return Some(embedded);
        }

        let floating = GuiConfiguration {
            api_type,
            is_floating: true,
        };
        if gui_ext.is_api_supported(handle, floating) {
            return Some(floating);
        }

        None
    }

    fn first_descriptor_name_and_id(plugin_path: &Path) -> Result<(String, String), String> {
        let bundle = unsafe { PluginBundle::load(plugin_path) }
            .map_err(|e| format!("clap load failed: {e}"))?;
        let (_, name, _) = select_descriptor(&bundle, plugin_path)?;
        let factory = bundle
            .get_plugin_factory()
            .ok_or_else(|| "clap missing plugin factory".to_string())?;
        let mut fallback_id = None;
        for desc in factory.plugin_descriptors() {
            let features = descriptor_features(&desc);
            if has_synth_like_feature(&features) {
                continue;
            }
            if let Some(id) = desc.id().and_then(|v| v.to_str().ok()) {
                return Ok((name, id.to_string()));
            }
            if fallback_id.is_none() {
                fallback_id = desc
                    .id()
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string());
            }
        }
        let id = fallback_id.ok_or_else(|| "clap descriptor id missing".to_string())?;
        Ok((name, id))
    }

    fn first_descriptor_features(plugin_path: &Path) -> Result<Vec<String>, String> {
        let bundle = unsafe { PluginBundle::load(plugin_path) }
            .map_err(|e| format!("clap load failed: {e}"))?;
        let factory = bundle
            .get_plugin_factory()
            .ok_or_else(|| "clap missing plugin factory".to_string())?;
        let mut fallback = Vec::new();
        for descriptor in factory.plugin_descriptors() {
            let features = descriptor_features(&descriptor);
            if has_synth_like_feature(&features) {
                if fallback.is_empty() {
                    fallback = features;
                }
                continue;
            }
            return Ok(features);
        }
        Ok(fallback)
    }

    pub(crate) fn is_audio_effect_plugin(plugin_path: &Path) -> Result<bool, String> {
        let features = first_descriptor_features(plugin_path)?;
        if has_synth_like_feature(&features) {
            return Ok(false);
        }
        if features.is_empty() {
            return Ok(true);
        }
        Ok(features.iter().any(|f| f == "audio-effect"))
    }

    pub(crate) fn gui_capabilities(plugin_path: &Path) -> GuiCapabilities {
        let Ok((_, mut instance, _, _)) = instantiate_plugin(plugin_path) else {
            return GuiCapabilities {
                supports_native_gui: false,
                supports_param_feedback: false,
                supports_state_sync: false,
            };
        };

        let mut handle = instance.plugin_handle();
        let params_ok = handle.get_extension::<PluginParams>().is_some();
        let state_ok = handle.get_extension::<PluginState>().is_some();
        let gui_ok = handle
            .get_extension::<PluginGui>()
            .and_then(|gui| negotiate_gui_configuration(&gui, &mut handle))
            .is_some();

        GuiCapabilities {
            supports_native_gui: gui_ok,
            supports_param_feedback: params_ok,
            supports_state_sync: state_ok,
        }
    }

    pub(crate) fn scan_paths(search_paths: &[String]) -> Result<Vec<PluginDescriptorInfo>, String> {
        let mut out = Vec::new();
        for plugin_path in candidate_bundles(search_paths) {
            if !is_audio_effect_plugin(&plugin_path).unwrap_or(true) {
                continue;
            }
            let Ok((name, _id)) = first_descriptor_name_and_id(&plugin_path) else {
                continue;
            };
            let path_str = plugin_path.to_string_lossy().to_string();
            out.push(PluginDescriptorInfo {
                key: path_str.clone(),
                name,
                path: path_str,
                format: PluginFormat::Clap,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out.dedup_by(|a, b| a.path == b.path);
        Ok(out)
    }

    pub(crate) fn probe(
        plugin_path: &Path,
    ) -> Result<(PluginDescriptorInfo, Vec<PluginParamInfo>, Option<String>), String> {
        if !is_audio_effect_plugin(plugin_path).unwrap_or(true) {
            return Err(format!(
                "instrument/synth plugins are excluded from Plugin FX ({})",
                plugin_path.display()
            ));
        }

        let (_bundle, mut instance, plugin_name, _features) = instantiate_plugin(plugin_path)?;
        let (mut params, _specs, _params_ext) = collect_params(&mut instance);
        if params.is_empty() {
            params = crate::plugin::backends::default_params();
        }

        let path_str = plugin_path.to_string_lossy().to_string();
        Ok((
            PluginDescriptorInfo {
                key: path_str.clone(),
                name: plugin_name,
                path: path_str,
                format: PluginFormat::Clap,
            },
            params,
            None,
        ))
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process(
        plugin_path: &Path,
        input_audio_path: &Path,
        output_audio_path: &Path,
        sample_rate: u32,
        max_block_size: usize,
        enabled: bool,
        bypass: bool,
        state_blob_b64: Option<&str>,
        params: &[PluginParamValue],
    ) -> Result<Option<String>, String> {
        if !is_audio_effect_plugin(plugin_path).unwrap_or(true) {
            return Err(format!(
                "instrument/synth plugins are excluded from Plugin FX ({})",
                plugin_path.display()
            ));
        }

        let (channels, input_sr) = crate::audio_io::decode_audio_multi(input_audio_path)
            .map_err(|e| format!("decode failed: {e}"))?;
        if channels.is_empty() {
            return Err("decode returned empty channels".to_string());
        }

        if !enabled || bypass {
            if let Some(parent) = output_audio_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            crate::wave::export_channels_audio(&channels, input_sr.max(1), output_audio_path)
                .map_err(|e| format!("encode failed: {e}"))?;
            return Ok(state_blob_b64.map(|s| s.to_string()));
        }

        let (_bundle, mut instance, _plugin_name, _features) = instantiate_plugin(plugin_path)?;
        let (ui_params, specs, params_ext) = collect_params(&mut instance);
        let state_ext = {
            let handle = instance.plugin_handle();
            handle.get_extension::<PluginState>()
        };

        maybe_load_state(&mut instance, state_ext, state_blob_b64)?;
        if let Some(params_ext) = params_ext {
            if !ui_params.is_empty() {
                apply_param_values_inactive(&mut instance, params_ext, &specs, params);
            }
        }

        let block_size = max_block_size.clamp(1, 4096);
        let processor = instance
            .activate(
                |_, _| (),
                PluginAudioConfiguration {
                    sample_rate: sample_rate.max(1) as f64,
                    min_frames_count: 1,
                    max_frames_count: block_size as u32,
                },
            )
            .map_err(|e| format!("clap activate failed: {e}"))?;
        let mut processor = processor
            .start_processing()
            .map_err(|e| format!("clap start_processing failed: {e}"))?;

        let channels_len = channels.len();
        let frames_total = channels[0].len();
        let mut out_channels = vec![vec![0.0f32; frames_total]; channels_len];
        let mut input_ports = AudioPorts::with_capacity(channels_len, 1);
        let mut output_ports = AudioPorts::with_capacity(channels_len, 1);

        let spec_map: HashMap<&str, &ClapParamSpec> =
            specs.iter().map(|s| (s.id.as_str(), s)).collect();
        let mut param_events = EventBuffer::with_capacity(params.len());
        for p in params {
            if let Some(spec) = spec_map.get(p.id.as_str()) {
                let plain = normalized_to_plain(p.normalized, spec.min_plain, spec.max_plain);
                param_events.push(&ParamValueEvent::new(
                    0,
                    spec.clap_id,
                    Pckn::match_all(),
                    plain,
                    Cookie::empty(),
                ));
            } else if let Some(clap_id) = parse_param_id(&p.id) {
                param_events.push(&ParamValueEvent::new(
                    0,
                    clap_id,
                    Pckn::match_all(),
                    p.normalized.clamp(0.0, 1.0) as f64,
                    Cookie::empty(),
                ));
            }
        }
        let mut send_param_events = !param_events.is_empty();

        let mut cursor = 0usize;
        while cursor < frames_total {
            let frames_now = (frames_total - cursor).min(block_size);
            let mut in_block: Vec<Vec<f32>> = channels
                .iter()
                .map(|ch| ch[cursor..cursor + frames_now].to_vec())
                .collect();
            let mut out_block: Vec<Vec<f32>> = vec![vec![0.0; frames_now]; channels_len];

            let mut output_events_buf = EventBuffer::new();
            let mut output_events = output_events_buf.as_output();

            let input_audio = input_ports.with_input_buffers([AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(in_block.iter_mut().map(|buf| {
                    InputChannel {
                        buffer: buf.as_mut_slice(),
                        is_constant: false,
                    }
                })),
            }]);
            let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    out_block.iter_mut().map(|buf| buf.as_mut_slice()),
                ),
            }]);

            let input_events = if send_param_events {
                send_param_events = false;
                param_events.as_input()
            } else {
                InputEvents::empty()
            };

            processor
                .process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events,
                    None,
                    None,
                )
                .map_err(|e| format!("clap process failed: {e}"))?;

            for (ci, out) in out_channels.iter_mut().enumerate() {
                out[cursor..cursor + frames_now].copy_from_slice(&out_block[ci]);
            }
            cursor += frames_now;
        }

        let stopped = processor.stop_processing();
        instance.deactivate(stopped);

        let next_state_blob_b64 = maybe_save_state(&mut instance, state_ext)
            .or_else(|| state_blob_b64.map(|s| s.to_string()));

        if let Some(parent) = output_audio_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        crate::wave::export_channels_audio(&out_channels, sample_rate.max(1), output_audio_path)
            .map_err(|e| format!("encode failed: {e}"))?;

        Ok(next_state_blob_b64)
    }

    pub(crate) struct GuiSession {
        _bundle: PluginBundle,
        instance: PluginInstance<ClapHostHandlers>,
        gui_ext: PluginGui,
        gui_open: bool,
        params_ext: Option<PluginParams>,
        state_ext: Option<PluginState>,
        param_specs: Vec<ClapParamSpec>,
        last_values: HashMap<u32, f32>,
        state_blob_b64: Option<String>,
    }

    pub(crate) fn gui_open(
        plugin_path: &Path,
        state_blob_b64: Option<&str>,
        params: &[PluginParamValue],
    ) -> Result<(GuiSession, Vec<PluginParamInfo>, Option<String>), String> {
        if !is_audio_effect_plugin(plugin_path).unwrap_or(true) {
            return Err(format!(
                "instrument/synth plugins are excluded from Plugin FX ({})",
                plugin_path.display()
            ));
        }

        let (bundle, mut instance, _plugin_name, _features) = instantiate_plugin(plugin_path)?;
        let (mut ui_params, specs, params_ext) = collect_params(&mut instance);
        let state_ext = {
            let handle = instance.plugin_handle();
            handle.get_extension::<PluginState>()
        };

        maybe_load_state(&mut instance, state_ext, state_blob_b64)?;
        if let Some(params_ext) = params_ext {
            if !specs.is_empty() {
                apply_param_values_inactive(&mut instance, params_ext, &specs, params);
            }
        }

        let mut handle = instance.plugin_handle();
        let gui_ext = handle
            .get_extension::<PluginGui>()
            .ok_or_else(|| "plugin does not expose CLAP GUI extension".to_string())?;
        let config = negotiate_gui_configuration(&gui_ext, &mut handle)
            .ok_or_else(|| "plugin GUI does not support this platform API".to_string())?;

        gui_ext
            .create(&mut handle, config)
            .map_err(|e| format!("gui create failed: {e}"))?;
        let title =
            CString::new("NeoWaves Plugin GUI").map_err(|e| format!("title failed: {e}"))?;
        gui_ext.suggest_title(&mut handle, title.as_c_str());
        gui_ext
            .show(&mut handle)
            .map_err(|e| format!("gui show failed: {e}"))?;

        let snapshot = read_snapshot(&mut instance, params_ext, &specs);
        let mut last_values = HashMap::new();
        for item in &snapshot {
            if let Some(pid) = parse_param_id(&item.id) {
                last_values.insert(pid.get(), item.normalized.clamp(0.0, 1.0));
            }
        }

        if ui_params.is_empty() {
            ui_params = crate::plugin::backends::default_params();
        }

        Ok((
            GuiSession {
                _bundle: bundle,
                instance,
                gui_ext,
                gui_open: true,
                params_ext,
                state_ext,
                param_specs: specs,
                last_values,
                state_blob_b64: state_blob_b64.map(|s| s.to_string()),
            },
            ui_params,
            state_blob_b64.map(|s| s.to_string()),
        ))
    }

    pub(crate) fn gui_poll(
        session: &mut GuiSession,
    ) -> Result<
        (
            Vec<PluginParamValue>,
            Option<Vec<PluginParamValue>>,
            Option<String>,
            bool,
        ),
        String,
    > {
        let callback_requested = session.instance.access_shared_handler(|shared| {
            shared.callback_requested.swap(false, Ordering::AcqRel)
        });
        if callback_requested {
            session.instance.call_on_main_thread_callback();
        }

        let flush_requested = session
            .instance
            .access_shared_handler(|shared| shared.flush_requested.swap(false, Ordering::AcqRel));
        if flush_requested {
            let snapshot = read_snapshot(
                &mut session.instance,
                session.params_ext,
                &session.param_specs,
            );
            for item in &snapshot {
                if let Some(pid) = parse_param_id(&item.id) {
                    session
                        .last_values
                        .insert(pid.get(), item.normalized.clamp(0.0, 1.0));
                }
            }
        }

        let mut deltas = Vec::new();
        for item in read_snapshot(
            &mut session.instance,
            session.params_ext,
            &session.param_specs,
        ) {
            let Some(pid) = parse_param_id(&item.id) else {
                continue;
            };
            let prev = session.last_values.get(&pid.get()).copied().unwrap_or(-1.0);
            if (item.normalized - prev).abs() > 0.000_01 {
                session
                    .last_values
                    .insert(pid.get(), item.normalized.clamp(0.0, 1.0));
                deltas.push(item);
            }
        }

        let state_dirty = session.instance.access_handler_mut(|handler| {
            let dirty = handler.state_dirty;
            handler.state_dirty = false;
            dirty
        });
        if state_dirty {
            if let Some(next_state) = maybe_save_state(&mut session.instance, session.state_ext) {
                session.state_blob_b64 = Some(next_state);
            }
        }

        let closed = session
            .instance
            .access_shared_handler(|shared| shared.gui_closed.load(Ordering::Acquire));
        if closed {
            let snapshot = read_snapshot(
                &mut session.instance,
                session.params_ext,
                &session.param_specs,
            );
            if let Some(next_state) = maybe_save_state(&mut session.instance, session.state_ext) {
                session.state_blob_b64 = Some(next_state);
            }
            return Ok((deltas, Some(snapshot), session.state_blob_b64.clone(), true));
        }

        Ok((deltas, None, session.state_blob_b64.clone(), false))
    }

    pub(crate) fn gui_close(
        mut session: GuiSession,
    ) -> Result<(Option<Vec<PluginParamValue>>, Option<String>), String> {
        let snapshot = read_snapshot(
            &mut session.instance,
            session.params_ext,
            &session.param_specs,
        );

        if session.gui_open {
            let mut handle = session.instance.plugin_handle();
            session.gui_ext.destroy(&mut handle);
            session.gui_open = false;
        }

        if let Some(next_state) = maybe_save_state(&mut session.instance, session.state_ext) {
            session.state_blob_b64 = Some(next_state);
        }

        Ok((Some(snapshot), session.state_blob_b64.take()))
    }
}

#[cfg(not(feature = "plugin_native_clap"))]
mod native {
    use super::*;

    pub(crate) fn scan_paths(
        _search_paths: &[String],
    ) -> Result<Vec<PluginDescriptorInfo>, String> {
        Err("native clap backend unavailable (build without plugin_native_clap)".to_string())
    }

    pub(crate) fn probe(
        _plugin_path: &Path,
    ) -> Result<(PluginDescriptorInfo, Vec<PluginParamInfo>, Option<String>), String> {
        Err("native clap backend unavailable".to_string())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process(
        _plugin_path: &Path,
        _input_audio_path: &Path,
        _output_audio_path: &Path,
        _sample_rate: u32,
        _max_block_size: usize,
        _enabled: bool,
        _bypass: bool,
        _state_blob_b64: Option<&str>,
        _params: &[PluginParamValue],
    ) -> Result<Option<String>, String> {
        Err("native clap backend unavailable".to_string())
    }

    pub(crate) fn is_audio_effect_plugin(_plugin_path: &Path) -> Result<bool, String> {
        Err("native clap backend unavailable".to_string())
    }

    pub(crate) fn gui_capabilities(_plugin_path: &Path) -> GuiCapabilities {
        GuiCapabilities {
            supports_native_gui: false,
            supports_param_feedback: false,
            supports_state_sync: false,
        }
    }

    pub(crate) struct GuiSession;

    pub(crate) fn gui_open(
        _plugin_path: &Path,
        _state_blob_b64: Option<&str>,
        _params: &[PluginParamValue],
    ) -> Result<(GuiSession, Vec<PluginParamInfo>, Option<String>), String> {
        Err("native clap backend unavailable".to_string())
    }

    pub(crate) fn gui_poll(
        _session: &mut GuiSession,
    ) -> Result<
        (
            Vec<PluginParamValue>,
            Option<Vec<PluginParamValue>>,
            Option<String>,
            bool,
        ),
        String,
    > {
        Err("native clap backend unavailable".to_string())
    }

    pub(crate) fn gui_close(
        _session: GuiSession,
    ) -> Result<(Option<Vec<PluginParamValue>>, Option<String>), String> {
        Err("native clap backend unavailable".to_string())
    }
}

pub(crate) use native::{
    gui_capabilities, gui_close, gui_open, gui_poll, is_audio_effect_plugin, probe, process,
    scan_paths, GuiSession,
};

#[cfg(test)]
mod tests {
    #[cfg(feature = "plugin_native_clap")]
    #[test]
    fn synth_feature_detection() {
        assert!(super::native::has_synth_like_feature(&[
            "instrument".to_string()
        ]));
        assert!(super::native::has_synth_like_feature(&[
            "audio-effect".to_string(),
            "synthesizer".to_string()
        ]));
        assert!(!super::native::has_synth_like_feature(&[
            "audio-effect".to_string()
        ]));
    }
}
