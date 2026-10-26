//! Check a `minimap.json` against a calibration: prints each field
//! point's residual after the projection fit, and writes `fit.ppm` - the
//! empty minimap with every field point drawn where the fit puts it.
//!
//! ```bash
//! cargo run -p reco-minimap --example minimap_fit -- Calibration.json minimap.json [out_dir]
//! ```

use std::path::{Path, PathBuf};

use reco_core::calibration::Calibration;
use reco_minimap::geom::Vec2;
use reco_minimap::reco::{MinimapSetup, RecoProjector};
use reco_minimap::render::{Renderer, Theme};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: minimap_fit <Calibration.json> <minimap.json> [out_dir]");
        std::process::exit(2);
    }
    let calibration = Calibration::from_file(Path::new(&args[0]))?;
    let setup = MinimapSetup::from_file(Path::new(&args[1]))?;
    let out_dir = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("."));
    let projector = RecoProjector::new(&calibration, &setup)?;
    println!("fit RMS: {:.2} m", projector.rms_error_m());

    let renderer = Renderer::with_profile(
        Theme {
            px_per_m: 10.0,
            ..Theme::default()
        },
        setup.sport_profile(),
    );
    let (w, h) = renderer.size();
    let mut canvas = reco_minimap::render::Canvas::new(w, h, [24, 92, 48], 255);
    renderer.draw_court(&mut canvas);
    let pad = renderer.theme.pad_m * (setup.field_length_m / 28.0).sqrt().max(0.8);
    let px = |p: Vec2| {
        (
            (p.x + pad) * renderer.theme.px_per_m,
            (p.y + pad) * renderer.theme.px_per_m,
        )
    };
    for (i, p) in setup.points.iter().enumerate() {
        let truth = Vec2::new(p.field_x, p.field_y);
        match projector.camera_to_field(p.camera.into(), p.x as f32, p.y as f32) {
            Some(m) => {
                println!(
                    "point {i} ({:?}): expected ({:.1}, {:.1}) fitted ({:.1}, {:.1}) residual {:.2} m",
                    p.camera,
                    truth.x,
                    truth.y,
                    m.x,
                    m.y,
                    m.dist(truth)
                );
                let (x, y) = px(m);
                canvas.disc(x, y, 6.0, 0.0, [255, 60, 60], 1.0);
            }
            None => println!("point {i} ({:?}): not projectable", p.camera),
        }
        let (x, y) = px(truth);
        canvas.disc(x, y, 9.0, 7.0, [255, 255, 0], 1.0);
    }
    let out = out_dir.join("fit.ppm");
    std::fs::write(&out, canvas.write_ppm())?;
    println!("wrote {}", out.display());
    Ok(())
}
