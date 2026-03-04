use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("debug").join("long_load_test.mp3"));
    let secs = args
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(180)
        .max(10);
    let sr = 48_000u32;
    let frames = (sr as usize).saturating_mul(secs as usize);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut noise = 0x1234_5678u32;
    for i in 0..frames {
        let t = i as f32 / sr as f32;
        noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let n = (((noise >> 8) & 0xffff) as f32 / 32768.0 - 1.0) * 0.035;
        let env = 0.65 + 0.25 * (2.0 * std::f32::consts::PI * 0.07 * t).sin();
        let l = ((2.0 * std::f32::consts::PI * 110.0 * t).sin() * 0.28
            + (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.18
            + (2.0 * std::f32::consts::PI * (330.0 + 20.0 * (0.3 * t).sin()) * t).sin() * 0.12)
            * env
            + n;
        let r = ((2.0 * std::f32::consts::PI * 146.83 * t).sin() * 0.26
            + (2.0 * std::f32::consts::PI * 293.66 * t).sin() * 0.16
            + (2.0 * std::f32::consts::PI * (440.0 + 24.0 * (0.23 * t).cos()) * t).sin() * 0.12)
            * env
            - n;
        left.push(l.clamp(-0.98, 0.98));
        right.push(r.clamp(-0.98, 0.98));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    neowaves::wave::export_channels_audio(&[left, right], sr, &out)?;
    println!("generated {}", out.display());
    Ok(())
}
