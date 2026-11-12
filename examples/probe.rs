// Manual probe: lists render endpoints and optionally cycles the default device.
//   cargo run --example probe            -> list
//   cargo run --example probe -- set ID  -> set default
#[path = "../src/audio.rs"]
#[allow(dead_code)]
mod audio;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

fn main() {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
    let a = audio::Audio::new().expect("init");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "set" {
        a.set_default(&args[1]).expect("set_default");
    }
    let cur = a.default_id();
    for d in a.devices() {
        let mark = if Some(&d.id) == cur.as_ref() { "*" } else { " " };
        println!("{mark} {} | {} | {}", d.name, d.icon_path, d.id);
    }
}
