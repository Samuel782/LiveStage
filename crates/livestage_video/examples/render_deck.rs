//! Renders a presentation the way the importer does.
//!
//! ```text
//! cargo run -p livestage_video --example render_deck -- <deck.pdf|pptx> <out dir> [WxH]
//! ```

use livestage_video::slides::{import_document, ImportOptions};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(src), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: render_deck <deck.pdf|pptx> <out dir> [WxH]");
        std::process::exit(2);
    };
    let target = args
        .get(2)
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1920, 1080));
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).expect("output dir");
    let opts = ImportOptions { target, ..Default::default() };
    println!("LibreOffice: {}", opts.office.as_ref().map_or("not found (built-in renderer)".into(), |p| p.display().to_string()));
    let t0 = std::time::Instant::now();
    let deck = import_document(src.as_ref(), None, &out, "slides", &opts, &mut |d, t| println!("slide {d}/{t}")).unwrap_or_else(|e| {
        eprintln!("import failed: {e}");
        std::process::exit(1);
    });
    println!("{} slides at {:?} in {:.2?}", deck.slides.len(), deck.size, t0.elapsed());
    for (i, s) in deck.slides.iter().enumerate() {
        println!("--- {} ---\n{}\nnotes: {}", i + 1, s.text, s.notes);
    }
}
