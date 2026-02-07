use std::path::Path;

use crate::plugin::protocol::{PluginDescriptorInfo, PluginParamInfo, PluginParamValue};

#[cfg(feature = "plugin_native_vst3")]
mod native {
    use super::*;
    use std::mem::ManuallyDrop;
    use std::ffi::c_void;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::ptr;

    use libloading::Library;
    use crate::plugin::backends::plugin_display_name;
    use crate::plugin::PluginFormat;
    use vst3::{Class, ComPtr, ComWrapper, Interface, Steinberg};
    use vst3::Steinberg::{IPluginBaseTrait, IPluginFactory2Trait, IPluginFactoryTrait};
    use vst3::Steinberg::Vst::{
        AudioBusBuffers, AudioBusBuffers__type0, IAudioProcessor, IAudioProcessorTrait, IComponent,
        IComponentHandler, IComponentHandlerTrait, IComponentTrait, IConnectionPoint, IConnectionPointTrait,
        IEditController, IEditControllerTrait, IHostApplication, IHostApplicationTrait, ParameterInfo, ProcessData, ProcessSetup,
        TChar,
    };

    fn debug_enabled() -> bool {
        std::env::var("NEOWAVES_PLUGIN_DEBUG")
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                !(v.is_empty() || v == "0" || v == "false" || v == "off")
            })
            .unwrap_or(false)
    }

    fn debug_log(msg: &str) {
        if debug_enabled() {
            eprintln!("[vst3] {msg}");
        }
    }

    fn copy_wstring(src: &str, dst: &mut [TChar]) {
        let mut len = 0usize;
        for (ch, slot) in src.encode_utf16().zip(dst.iter_mut()) {
            *slot = ch as TChar;
            len += 1;
        }
        if len < dst.len() {
            dst[len] = 0;
        } else if let Some(last) = dst.last_mut() {
            *last = 0;
        }
    }

    struct HostApplication;
    impl Class for HostApplication {
        type Interfaces = (IHostApplication,);
    }
    impl IHostApplicationTrait for HostApplication {
        unsafe fn getName(&self, name: *mut Steinberg::Vst::String128) -> Steinberg::tresult {
            if name.is_null() {
                return Steinberg::kInvalidArgument;
            }
            let name_buf: &mut Steinberg::Vst::String128 = &mut *name;
            copy_wstring("NeoWaves", &mut name_buf[..]);
            Steinberg::kResultOk
        }

        unsafe fn createInstance(
            &self,
            _cid: *mut Steinberg::TUID,
            _iid: *mut Steinberg::TUID,
            _obj: *mut *mut std::ffi::c_void,
        ) -> Steinberg::tresult {
            Steinberg::kNoInterface
        }
    }

    struct ComponentHandler;
    impl Class for ComponentHandler {
        type Interfaces = (IComponentHandler,);
    }
    impl IComponentHandlerTrait for ComponentHandler {
        unsafe fn beginEdit(&self, _id: Steinberg::Vst::ParamID) -> Steinberg::tresult {
            Steinberg::kResultOk
        }
        unsafe fn performEdit(
            &self,
            _id: Steinberg::Vst::ParamID,
            _value_normalized: Steinberg::Vst::ParamValue,
        ) -> Steinberg::tresult {
            Steinberg::kResultOk
        }
        unsafe fn endEdit(&self, _id: Steinberg::Vst::ParamID) -> Steinberg::tresult {
            Steinberg::kResultOk
        }
        unsafe fn restartComponent(&self, _flags: Steinberg::int32) -> Steinberg::tresult {
            Steinberg::kResultOk
        }
    }

    fn ensure_vst3_link() {
        let _ = <Steinberg::IPluginFactory as Interface>::IID;
    }

    fn candidate_binaries(plugin_path: &Path) -> Vec<PathBuf> {
        if plugin_path.is_file() {
            return vec![plugin_path.to_path_buf()];
        }
        if !plugin_path.is_dir() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(plugin_path)
            .follow_links(false)
            .max_depth(8)
            .into_iter()
            .filter_map(Result::ok)
        {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let ext = p
                .extension()
                .and_then(OsStr::to_str)
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if ext == "vst3" || ext == "dll" || ext == "so" || ext == "dylib" {
                out.push(p.to_path_buf());
            }
        }
        out
    }

    fn has_plugin_factory(binary: &Path) -> bool {
        ensure_vst3_link();
        unsafe {
            let Ok(lib) = Library::new(binary) else {
                return false;
            };
            lib.get::<*const ()>(b"GetPluginFactory\0").is_ok()
        }
    }

    fn find_valid_binary(plugin_path: &Path) -> Option<PathBuf> {
        candidate_binaries(plugin_path)
            .into_iter()
            .find(|bin| has_plugin_factory(bin))
    }

    fn c_char_array_to_string(bytes: &[Steinberg::char8]) -> String {
        let end = bytes.iter().position(|&v| v == 0).unwrap_or(bytes.len());
        let out: Vec<u8> = bytes[..end].iter().map(|&v| v as u8).collect();
        String::from_utf8_lossy(&out).trim().to_string()
    }

    pub(super) fn is_synth_like_subcategory(raw: &str) -> bool {
        let tokens: Vec<String> = raw
            .split('|')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if tokens.is_empty() {
            return false;
        }
        if tokens.first().map(|s| s == "instrument").unwrap_or(false) {
            return true;
        }
        tokens.iter().any(|t| {
            matches!(
                t.as_str(),
                "synth"
                    | "synthesizer"
                    | "sampler"
                    | "drum"
                    | "drum machine"
                    | "drum-machine"
                    | "musicalinstrument"
            )
        })
    }

    fn tchar_array_to_string(chars: &[TChar]) -> String {
        let end = chars.iter().position(|&v| v == 0).unwrap_or(chars.len());
        String::from_utf16_lossy(&chars[..end]).trim().to_string()
    }

    struct LoadedFactory {
        _lib: Library,
        factory: ComPtr<Steinberg::IPluginFactory>,
    }

    type GetPluginFactoryFn = unsafe extern "system" fn() -> *mut Steinberg::IPluginFactory;

    fn load_factory(binary: &Path) -> Result<LoadedFactory, String> {
        ensure_vst3_link();
        debug_log(&format!("load_factory begin: {}", binary.display()));
        unsafe {
            let lib = Library::new(binary)
                .map_err(|e| format!("vst3 load failed ({}): {e}", binary.display()))?;
            #[cfg(windows)]
            {
                type InitFn = unsafe extern "system" fn() -> bool;
                for sym in [b"InitDll\0".as_slice(), b"InitModule\0".as_slice()] {
                    if let Ok(init) = lib.get::<InitFn>(sym) {
                        debug_log(&format!(
                            "calling {}",
                            String::from_utf8_lossy(&sym[..sym.len().saturating_sub(1)])
                        ));
                        if !(*init)() {
                            return Err(format!(
                                "{} failed ({})",
                                String::from_utf8_lossy(&sym[..sym.len().saturating_sub(1)]),
                                binary.display()
                            ));
                        }
                    }
                }
            }
            let get_factory: libloading::Symbol<'_, GetPluginFactoryFn> = lib
                .get(b"GetPluginFactory\0")
                .map_err(|e| format!("GetPluginFactory not found ({}): {e}", binary.display()))?;
            debug_log("calling GetPluginFactory");
            let raw = get_factory();
            let factory = ComPtr::from_raw(raw)
                .ok_or_else(|| format!("GetPluginFactory returned null ({})", binary.display()))?;
            debug_log("load_factory ok");
            Ok(LoadedFactory { _lib: lib, factory })
        }
    }

    unsafe fn create_raw_instance(
        factory: &ComPtr<Steinberg::IPluginFactory>,
        class_id: &Steinberg::TUID,
        iid: &Steinberg::TUID,
    ) -> Result<*mut c_void, String> {
        let mut out = ptr::null_mut();
        let r = factory.createInstance(
            class_id.as_ptr() as Steinberg::FIDString,
            iid.as_ptr() as Steinberg::FIDString,
            &mut out,
        );
        if r != Steinberg::kResultOk || out.is_null() {
            return Err(format!(
                "createInstance failed (result={r}, class={})",
                hex_tuid(class_id)
            ));
        }
        Ok(out)
    }

    unsafe fn create_controller_from_class(
        factory: &ComPtr<Steinberg::IPluginFactory>,
        class_id: &Steinberg::TUID,
    ) -> Option<ComPtr<IEditController>> {
        let raw = create_raw_instance(factory, class_id, &Steinberg::Vst::IEditController_iid).ok()?;
        ComPtr::from_raw(raw as *mut IEditController)
    }

    unsafe fn create_component_from_class(
        factory: &ComPtr<Steinberg::IPluginFactory>,
        class_id: &Steinberg::TUID,
    ) -> Option<ComPtr<IComponent>> {
        let raw = create_raw_instance(factory, class_id, &Steinberg::Vst::IComponent_iid).ok()?;
        ComPtr::from_raw(raw as *mut IComponent)
    }

    fn hex_tuid(id: &Steinberg::TUID) -> String {
        let mut out = String::with_capacity(id.len() * 2);
        for b in id {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{:02x}", *b as u8));
        }
        out
    }

    fn parse_param_id(id: &str) -> Option<Steinberg::Vst::ParamID> {
        if let Some(hex) = id.strip_prefix("vst3:") {
            return u32::from_str_radix(hex, 16).ok();
        }
        id.parse::<u32>().ok()
    }

    fn speaker_arrangement_for_channels(channels: usize) -> Steinberg::Vst::SpeakerArrangement {
        match channels {
            0 | 1 => Steinberg::Vst::SpeakerArr::kMono,
            _ => Steinberg::Vst::SpeakerArr::kStereo,
        }
    }

    unsafe fn find_component_and_controller_cids(
        factory: &ComPtr<Steinberg::IPluginFactory>,
    ) -> Result<(Steinberg::TUID, Option<Steinberg::TUID>), String> {
        let class_count = factory.countClasses().max(0) as usize;
        if class_count == 0 {
            return Err("vst3 factory has no classes".to_string());
        }
        let mut component_cid: Option<Steinberg::TUID> = None;
        let mut controller_cid: Option<Steinberg::TUID> = None;
        let mut first_cid: Option<Steinberg::TUID> = None;
        for idx in 0..class_count {
            let mut info: Steinberg::PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(idx as i32, &mut info) != Steinberg::kResultOk {
                continue;
            }
            if first_cid.is_none() {
                first_cid = Some(info.cid);
            }
            let category = c_char_array_to_string(&info.category).to_ascii_lowercase();
            if component_cid.is_none() && category.contains("audio module") {
                component_cid = Some(info.cid);
            }
            if controller_cid.is_none() && category.contains("component controller") {
                controller_cid = Some(info.cid);
            }
        }
        let cid = component_cid.or(first_cid).ok_or_else(|| "no VST3 class found".to_string())?;
        Ok((cid, controller_cid))
    }

    unsafe fn collect_controller_params(controller: &ComPtr<IEditController>) -> Vec<PluginParamInfo> {
        let count = controller.getParameterCount().max(0) as usize;
        let mut out = Vec::with_capacity(count);
        for idx in 0..count {
            let mut info: ParameterInfo = std::mem::zeroed();
            let r = controller.getParameterInfo(idx as i32, &mut info);
            if r != Steinberg::kResultOk {
                continue;
            }

            let mut default_normalized = info.defaultNormalizedValue as f32;
            if !default_normalized.is_finite() {
                default_normalized = 0.0;
            }
            default_normalized = default_normalized.clamp(0.0, 1.0);

            let normalized = default_normalized;
            let (min, max) = if info.stepCount > 0 {
                (0.0, info.stepCount as f32)
            } else {
                (0.0, 1.0)
            };

            let mut name = tchar_array_to_string(&info.title);
            if name.is_empty() {
                name = tchar_array_to_string(&info.shortTitle);
            }
            if name.is_empty() {
                name = format!("Param {}", info.id);
            }
            let unit = tchar_array_to_string(&info.units);
            out.push(PluginParamInfo {
                id: format!("vst3:{:08x}", info.id),
                name,
                normalized,
                default_normalized,
                min,
                max,
                unit,
            });
        }
        out
    }

    fn collect_params_from_binary(binary: &Path) -> Result<(Option<String>, Vec<PluginParamInfo>), String> {
        debug_log(&format!("collect_params begin: {}", binary.display()));
        let loaded = ManuallyDrop::new(load_factory(binary)?);
        unsafe {
            let class_count = loaded.factory.countClasses().max(0) as usize;
            debug_log(&format!("class_count={class_count}"));
            if class_count == 0 {
                return Err("vst3 factory has no classes".to_string());
            }
            let mut first_class_name: Option<String> = None;
            let mut first_any: Option<Vec<PluginParamInfo>> = None;

            for idx in 0..class_count {
                let mut class_info: Steinberg::PClassInfo = std::mem::zeroed();
                let info_r = loaded.factory.getClassInfo(idx as i32, &mut class_info);
                if info_r != Steinberg::kResultOk {
                    debug_log(&format!("getClassInfo({idx}) failed: {info_r}"));
                    continue;
                }
                let category = c_char_array_to_string(&class_info.category);
                let category_lc = category.to_ascii_lowercase();
                debug_log(&format!("class[{idx}] category='{category}'"));
                if first_class_name.is_none() {
                    let class_name = c_char_array_to_string(&class_info.name);
                    if !class_name.is_empty() {
                        first_class_name = Some(class_name);
                    }
                }

                if let Some(component) = create_component_from_class(&loaded.factory, &class_info.cid) {
                    debug_log(&format!("class[{idx}] create_component ok"));
                    if let Some(controller) = component.cast::<IEditController>() {
                        debug_log(&format!("class[{idx}] cast component->controller ok"));
                        let params = collect_controller_params(&controller);
                        debug_log(&format!("class[{idx}] cast params={}", params.len()));
                        if !params.is_empty() {
                            std::mem::forget(controller);
                            std::mem::forget(component);
                            return Ok((first_class_name, params));
                        }
                        if first_any.is_none() {
                            first_any = Some(params);
                        }
                    }
                }

                let controller_like = category_lc.contains("controller");
                if controller_like {
                    if let Some(controller) = create_controller_from_class(&loaded.factory, &class_info.cid) {
                        debug_log(&format!("class[{idx}] create_controller(direct) ok"));
                        let params = collect_controller_params(&controller);
                        debug_log(&format!("class[{idx}] direct params={}", params.len()));
                        if !params.is_empty() {
                            std::mem::forget(controller);
                            return Ok((first_class_name, params));
                        }
                        if first_any.is_none() {
                            first_any = Some(params);
                        }
                    }
                }
            }

            if let Some(params) = first_any {
                return Ok((first_class_name, params));
            }
        }
        Err(format!(
            "vst3 probe failed: no IEditController in {}",
            binary.display()
        ))
    }

    pub(crate) fn scan_paths(search_paths: &[String]) -> Result<Vec<PluginDescriptorInfo>, String> {
        let mut out = Vec::new();
        for raw in search_paths {
            let path = Path::new(raw);
            if !path.exists() {
                continue;
            }
            let walker = walkdir::WalkDir::new(path)
                .follow_links(false)
                .max_depth(8)
                .into_iter();
            for entry in walker.filter_map(Result::ok) {
                let p = entry.path();
                let ext = p
                    .extension()
                    .and_then(OsStr::to_str)
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default();
                if ext != "vst3" {
                    continue;
                }
                if find_valid_binary(p).is_some()
                    && is_audio_effect_plugin(p).unwrap_or(true)
                {
                    let path_str = p.to_string_lossy().to_string();
                    out.push(PluginDescriptorInfo {
                        key: path_str.clone(),
                        name: plugin_display_name(p),
                        path: path_str,
                        format: PluginFormat::Vst3,
                    });
                }
            }
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
        let binary = find_valid_binary(plugin_path).ok_or_else(|| {
            format!(
                "vst3 probe failed: GetPluginFactory not found ({})",
                plugin_path.display()
            )
        })?;
        let (native_name, params) = collect_params_from_binary(&binary)?;
        let path_str = plugin_path.to_string_lossy().to_string();
        Ok((
            PluginDescriptorInfo {
                key: path_str.clone(),
                name: native_name
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| plugin_display_name(plugin_path)),
                path: path_str,
                format: PluginFormat::Vst3,
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
        let (in_channels, input_sr) = crate::audio_io::decode_audio_multi(input_audio_path)
            .map_err(|e| format!("decode failed: {e}"))?;
        if in_channels.is_empty() {
            return Err("decode returned empty channels".to_string());
        }
        if !enabled || bypass {
            if let Some(parent) = output_audio_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            crate::wave::export_channels_audio(&in_channels, sample_rate.max(input_sr).max(1), output_audio_path)
                .map_err(|e| format!("encode failed: {e}"))?;
            return Ok(state_blob_b64.map(|v| v.to_string()));
        }

        let binary = find_valid_binary(plugin_path)
            .ok_or_else(|| format!("vst3 process failed: GetPluginFactory not found ({})", plugin_path.display()))?;

        let mut out_channels = in_channels.clone();
        let frame_count = out_channels.get(0).map(|c| c.len()).unwrap_or(0);
        let ch_count = out_channels.len().max(1);
        let block = max_block_size.clamp(16, 4096);

        unsafe {
            let loaded = ManuallyDrop::new(load_factory(&binary)?);
            let (component_cid, controller_cid) = find_component_and_controller_cids(&loaded.factory)?;

            let host = ComWrapper::new(HostApplication)
                .to_com_ptr::<IHostApplication>()
                .ok_or_else(|| "failed to create IHostApplication".to_string())?;
            let handler = ComWrapper::new(ComponentHandler)
                .to_com_ptr::<IComponentHandler>()
                .ok_or_else(|| "failed to create IComponentHandler".to_string())?;

            let component = create_component_from_class(&loaded.factory, &component_cid)
                .ok_or_else(|| "failed to create VST3 IComponent".to_string())?;
            let init_r = component.initialize(host.as_ptr() as *mut Steinberg::FUnknown);
            if init_r != Steinberg::kResultOk && init_r != Steinberg::kResultTrue {
                return Err(format!("component.initialize failed: {init_r}"));
            }
            let processor = component
                .cast::<IAudioProcessor>()
                .ok_or_else(|| "component does not implement IAudioProcessor".to_string())?;

            let mut controller_from_component = true;
            let controller = if let Some(ctrl) = component.cast::<IEditController>() {
                ctrl
            } else if let Some(cid) = controller_cid {
                controller_from_component = false;
                let ctrl = create_controller_from_class(&loaded.factory, &cid)
                    .ok_or_else(|| "failed to create VST3 IEditController".to_string())?;
                let r = ctrl.initialize(host.as_ptr() as *mut Steinberg::FUnknown);
                if r != Steinberg::kResultOk && r != Steinberg::kResultTrue {
                    return Err(format!("controller.initialize failed: {r}"));
                }
                ctrl
            } else {
                return Err("controller not available".to_string());
            };

            let _ = controller.setComponentHandler(handler.as_ptr());

            if let (Some(comp_cp), Some(ctrl_cp)) = (
                component.cast::<IConnectionPoint>(),
                controller.cast::<IConnectionPoint>(),
            ) {
                let _ = comp_cp.connect(ctrl_cp.as_ptr());
                let _ = ctrl_cp.connect(comp_cp.as_ptr());
                std::mem::forget(comp_cp);
                std::mem::forget(ctrl_cp);
            }

            for p in params {
                if let Some(pid) = parse_param_id(&p.id) {
                    let v = p.normalized.clamp(0.0, 1.0) as f64;
                    let _ = controller.setParamNormalized(pid, v);
                }
            }

            let mut in_arr = speaker_arrangement_for_channels(ch_count);
            let mut out_arr = in_arr;
            let _ = processor.setBusArrangements(&mut in_arr, 1, &mut out_arr, 1);
            let _ = component.setIoMode(Steinberg::Vst::IoModes_::kOfflineProcessing as i32);
            let _ = component.activateBus(
                Steinberg::Vst::MediaTypes_::kAudio as i32,
                Steinberg::Vst::BusDirections_::kInput as i32,
                0,
                1,
            );
            let _ = component.activateBus(
                Steinberg::Vst::MediaTypes_::kAudio as i32,
                Steinberg::Vst::BusDirections_::kOutput as i32,
                0,
                1,
            );

            let mut setup = ProcessSetup {
                processMode: Steinberg::Vst::ProcessModes_::kOffline as i32,
                symbolicSampleSize: Steinberg::Vst::SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: block as i32,
                sampleRate: sample_rate.max(1) as f64,
            };
            let setup_r = processor.setupProcessing(&mut setup);
            if setup_r != Steinberg::kResultOk && setup_r != Steinberg::kResultTrue {
                return Err(format!("processor.setupProcessing failed: {setup_r}"));
            }

            let _ = component.setActive(1);
            let _ = processor.setProcessing(1);

            let mut rendered = vec![vec![0.0f32; frame_count]; ch_count];
            let mut cursor = 0usize;
            while cursor < frame_count {
                let frames_now = (frame_count - cursor).min(block);
                let mut in_block: Vec<Vec<f32>> = out_channels
                    .iter()
                    .map(|ch| ch[cursor..cursor + frames_now].to_vec())
                    .collect();
                let mut out_block: Vec<Vec<f32>> = vec![vec![0.0f32; frames_now]; ch_count];
                let mut in_ptrs: Vec<*mut f32> = in_block.iter_mut().map(|ch| ch.as_mut_ptr()).collect();
                let mut out_ptrs: Vec<*mut f32> = out_block.iter_mut().map(|ch| ch.as_mut_ptr()).collect();

                let mut in_bus = AudioBusBuffers {
                    numChannels: ch_count as i32,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: in_ptrs.as_mut_ptr(),
                    },
                };
                let mut out_bus = AudioBusBuffers {
                    numChannels: ch_count as i32,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: out_ptrs.as_mut_ptr(),
                    },
                };
                let mut data = ProcessData {
                    processMode: setup.processMode,
                    symbolicSampleSize: setup.symbolicSampleSize,
                    numSamples: frames_now as i32,
                    numInputs: 1,
                    numOutputs: 1,
                    inputs: &mut in_bus,
                    outputs: &mut out_bus,
                    inputParameterChanges: ptr::null_mut(),
                    outputParameterChanges: ptr::null_mut(),
                    inputEvents: ptr::null_mut(),
                    outputEvents: ptr::null_mut(),
                    processContext: ptr::null_mut(),
                };
                let pr = processor.process(&mut data);
                if pr != Steinberg::kResultOk && pr != Steinberg::kResultTrue {
                    return Err(format!("processor.process failed: {pr}"));
                }
                for (ci, ch) in rendered.iter_mut().enumerate() {
                    ch[cursor..cursor + frames_now].copy_from_slice(&out_block[ci]);
                }
                cursor += frames_now;
            }

            let _ = processor.setProcessing(0);
            let _ = component.setActive(0);
            if !controller_from_component {
                let _ = controller.terminate();
            }
            let _ = component.terminate();

            out_channels = rendered;

            // Avoid plugin-specific shutdown crashes on worker process teardown.
            std::mem::forget(host);
            std::mem::forget(handler);
            std::mem::forget(controller);
            std::mem::forget(processor);
            std::mem::forget(component);
        }

        if let Some(parent) = output_audio_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        crate::wave::export_channels_audio(&out_channels, sample_rate.max(1), output_audio_path)
            .map_err(|e| format!("encode failed: {e}"))?;

        Ok(state_blob_b64.map(|v| v.to_string()))
    }

    pub(crate) fn is_audio_effect_plugin(plugin_path: &Path) -> Result<bool, String> {
        let binary = find_valid_binary(plugin_path).ok_or_else(|| {
            format!(
                "vst3 classify failed: GetPluginFactory not found ({})",
                plugin_path.display()
            )
        })?;
        let loaded = ManuallyDrop::new(load_factory(&binary)?);
        unsafe {
            if let Some(factory2) = loaded.factory.cast::<Steinberg::IPluginFactory2>() {
                let class_count = factory2.countClasses().max(0) as usize;
                for idx in 0..class_count {
                    let mut info2: Steinberg::PClassInfo2 = std::mem::zeroed();
                    if factory2.getClassInfo2(idx as i32, &mut info2) != Steinberg::kResultOk {
                        continue;
                    }
                    let category = c_char_array_to_string(&info2.category).to_ascii_lowercase();
                    if !category.contains("audio module") {
                        continue;
                    }
                    let sub = c_char_array_to_string(&info2.subCategories);
                    if is_synth_like_subcategory(&sub) {
                        return Ok(false);
                    }
                    return Ok(true);
                }
            }
        }
        Ok(true)
    }
}

#[cfg(not(feature = "plugin_native_vst3"))]
mod native {
    use super::*;

    pub(crate) fn scan_paths(_search_paths: &[String]) -> Result<Vec<PluginDescriptorInfo>, String> {
        Err("native vst3 backend unavailable (build without plugin_native_vst3)".to_string())
    }

    pub(crate) fn probe(
        _plugin_path: &Path,
    ) -> Result<(PluginDescriptorInfo, Vec<PluginParamInfo>, Option<String>), String> {
        Err("native vst3 backend unavailable".to_string())
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
        Err("native vst3 backend unavailable".to_string())
    }

    pub(crate) fn is_audio_effect_plugin(_plugin_path: &Path) -> Result<bool, String> {
        Err("native vst3 backend unavailable".to_string())
    }
}

pub(crate) use native::{is_audio_effect_plugin, probe, process, scan_paths};

#[cfg(test)]
mod tests {
    #[cfg(feature = "plugin_native_vst3")]
    #[test]
    fn synth_subcategory_detection() {
        assert!(super::native::is_synth_like_subcategory("Instrument|Synth"));
        assert!(super::native::is_synth_like_subcategory("Instrument"));
        assert!(!super::native::is_synth_like_subcategory("Fx|Dynamics"));
        assert!(!super::native::is_synth_like_subcategory("Fx|Instrument"));
    }
}
