//! Print `reco_io::ffmpeg::media_info::probe_media_info` for video files.
//!
//! Usage:
//!   cargo run -p reco-io --example media_info -- <video>...

use std::path::Path;

fn main() {
    reco_io::init();
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("Usage: media_info <video>...");
        std::process::exit(1);
    }
    for p in &paths {
        match reco_io::ffmpeg::media_info::probe_media_info(Path::new(p)) {
            Ok(info) => println!("== {p}\n{info:#?}"),
            Err(e) => eprintln!("{p}: {e}"),
        }
    }
}
