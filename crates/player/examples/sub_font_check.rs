//! Dev tool (Milestone 8, B8): renders subtitle frames to PNG files with the app's own subtitle
//! font setup (`subtitle_font_options`), so non-Latin text can be checked for missing-glyph
//! boxes without capturing any window: mpv's `vo=image` draws the subtitles into each frame.
//!
//! cargo run -p syncwatch-player --example sub_font_check -- <video> <subtitle> <fonts_dir> <out_dir> [app|nofallback]
//!
//! `app` (default) = what the app does: bundled Noto Sans, then the system fonts.
//! `nofallback` = the control: system fonts disabled (`sub-font-provider=none`), so only the
//! bundled Noto Sans is available and scripts it lacks must show boxes or nothing.
//! (libmpv-2.dll must be found: put LIBMPV_DIR on PATH for this run.)
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use libmpv2::Mpv;
use libmpv2::events::Event;
use std::path::PathBuf;
use syncwatch_player::mpv::subtitle_font_options;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 4 {
        println!("usage: sub_font_check <video> <subtitle> <fonts_dir> <out_dir> [app|nofallback]");
        std::process::exit(2);
    }
    let (video, sub, fonts, out) = (&a[0], &a[1], PathBuf::from(&a[2]), &a[3]);
    let mode = a.get(4).map(String::as_str).unwrap_or("app");
    std::fs::create_dir_all(out).unwrap();
    let mpv = Mpv::with_initializer(|i| {
        for (k, v) in [
            ("vo", "image"),
            ("vo-image-format", "png"),
            ("vo-image-outdir", out.as_str()),
            ("ao", "null"),
            ("hwdec", "no"),
            ("load-scripts", "no"),
            ("ytdl", "no"),
            ("terminal", "no"),
            ("idle", "no"),
        ] {
            i.set_option(k, v)?;
        }
        for (k, v) in subtitle_font_options(Some(&fonts)) {
            i.set_option(k, v.as_str())?;
        }
        if mode == "nofallback" {
            i.set_option("sub-font-provider", "none")?;
        }
        Ok(())
    })
    .unwrap();
    let opt = format!("sub-files-append={sub}");
    mpv.command("loadfile", &[video.as_str(), "replace", "-1", &opt])
        .unwrap();
    loop {
        match mpv.wait_event(30.0) {
            Some(Ok(Event::EndFile(r))) => {
                println!("{mode}: done ({r:?}), frames in {out}");
                break;
            }
            Some(Ok(_)) | Some(Err(_)) => {}
            None => {
                println!("{mode}: timed out");
                std::process::exit(1);
            }
        }
    }
}
