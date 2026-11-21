//! Saving and copying the frame on screen, and the small helpers around it: where the files
//! go and what they are called, showing a file in the file manager, a link to a moment.

use crate::app::App;
use crate::i18n::{Tr, t, tf};
use crate::jobs::Msg;
use crate::video;
use eframe::egui;
use gp_core::Ms;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A frame saved as the window shows it: subtitles drawn by the player included, the control
/// bar and anything else over the picture left out.
pub struct WindowShot {
    request: u64,
    path: PathBuf,
    phase: ShotPhase,
    capture: video::Capture,
    frames: u32,
}

#[derive(PartialEq)]
enum ShotPhase {
    /// Asked for during this frame, which may still show the menu.
    Asked,
    /// The next frame is drawn bare and read back at its end.
    Read,
    Reading,
}

impl App {
    /// Starts saving the frame on screen into `path`, or a new file in the frames folder.
    /// Returns the request, which `frame_done` names when the file is written.
    pub fn save_frame(&mut self, subtitles: bool, path: Option<PathBuf>) -> Result<u64, String> {
        let s = self.session.as_ref().ok_or("no file is open")?;
        let path = match path {
            Some(p) => p,
            None => {
                let dir = folder(&self.cfg.player.frames_dir);
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                unique_path(&dir, &file_name(&s.title, self.snap.pos))
            }
        };
        self.frame_requests += 1;
        let request = self.frame_requests;
        if subtitles {
            self.window_shot = Some(WindowShot {
                request,
                path,
                phase: ShotPhase::Asked,
                capture: Default::default(),
                frames: 0,
            });
            self.jobs.context().request_repaint();
        } else {
            let mpv = self.player.mpv.clone();
            self.jobs.spawn(move || Msg::FrameSaved {
                request,
                result: mpv
                    .command(&["screenshot-to-file", &path.to_string_lossy(), "video"])
                    .map(|()| path)
                    .map_err(|e| e.to_string()),
            });
        }
        Ok(request)
    }

    /// Puts the video frame, at its own size and without subtitles, on the clipboard.
    pub fn copy_frame(&mut self) {
        if self.session.is_none() {
            return;
        }
        self.frame_requests += 1;
        let file = std::env::temp_dir().join(format!(
            "glossplay-frame-{}-{}.png",
            std::process::id(),
            self.frame_requests
        ));
        let mpv = self.player.mpv.clone();
        self.jobs.spawn(move || {
            let result = mpv
                .command(&["screenshot-to-file", &file.to_string_lossy(), "video"])
                .map_err(|e| e.to_string())
                .and_then(|()| read_png(&file));
            let _ = std::fs::remove_file(&file);
            Msg::FrameCopied(result)
        });
    }

    pub fn on_frame_saved(&mut self, request: u64, result: Result<PathBuf, String>) {
        match &result {
            Ok(path) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                self.toast_for(tf(Tr::FrameSaved, &[&name]), Duration::from_secs(8));
                if self.status.is_some() {
                    self.toast_reveal = Some(path.clone());
                }
            }
            Err(e) => self.toast(tf(Tr::FrameNotSaved, &[e])),
        }
        self.frame_done = Some((request, result));
    }

    pub fn on_frame_copied(&mut self, result: Result<(u32, u32, Vec<u8>), String>) {
        match result {
            Ok((w, h, rgba)) => {
                if !self.quiet {
                    let image =
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    self.jobs.context().copy_image(image);
                }
                self.toast_for(t(Tr::FrameCopied), Duration::from_secs(3));
            }
            Err(e) => self.toast(tf(Tr::FrameNotSaved, &[&e])),
        }
    }

    pub fn on_ocr_done(&mut self, request: u64, result: Result<Vec<OcrLine>, String>) {
        match result {
            Ok(lines) if !lines.is_empty() => {
                self.ocr_result = Some((request, lines));
                self.toast(t(Tr::OcrResultTitle));
            }
            Ok(_) => {
                self.toast(t(Tr::OcrNoTextFound));
            }
            Err(e) => {
                self.toast(tf(Tr::OcrReadFailed, &[&e]));
            }
        }
    }
}

/// Runs at the end of every frame: moves a window shot along.
pub fn step(app: &mut App, ctx: &egui::Context) {
    let Some(shot) = app.window_shot.as_mut() else {
        return;
    };
    shot.frames += 1;
    ctx.request_repaint();
    match shot.phase {
        ShotPhase::Asked => shot.phase = ShotPhase::Read,
        ShotPhase::Read => {
            let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::unique("frame-shot"));
            ctx.layer_painter(layer).add(video::capture_callback(
                ctx.content_rect(),
                shot.capture.clone(),
            ));
            shot.phase = ShotPhase::Reading;
        }
        ShotPhase::Reading => {
            let pixels = shot
                .capture
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let Some((w, h, pixels)) = pixels else {
                if shot.frames > 30 {
                    let request = shot.request;
                    app.window_shot = None;
                    app.on_frame_saved(request, Err("the window was not drawn".into()));
                }
                return;
            };
            let Some(shot) = app.window_shot.take() else {
                return;
            };
            let ppp = ctx.pixels_per_point();
            let r = app.stage_rect;
            let px = |v: f32| (v * ppp).round().max(0.0) as u32;
            let rect = [px(r.left()), px(r.top()), px(r.right()), px(r.bottom())];
            let (request, path) = (shot.request, shot.path);
            app.jobs.spawn(move || {
                let result = match crop(w, h, &pixels, rect) {
                    Some((cw, ch, rgba)) => write_png(&path, cw, ch, &rgba)
                        .map(|()| path.clone())
                        .map_err(|e| format!("{}: {e}", path.display())),
                    None => Err("the picture is empty".into()),
                };
                Msg::FrameSaved { request, result }
            });
        }
    }
}

/// The folder for saved frames: the setting, or "Pictures/Glossplay".
pub fn folder(setting: &str) -> PathBuf {
    let setting = setting.trim();
    if !setting.is_empty() {
        return PathBuf::from(setting);
    }
    default_folder()
}

pub fn default_folder() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.picture_dir().map(|p| p.join("Glossplay")))
        .unwrap_or_else(|| crate::paths::data_dir().join("frames"))
}

/// "Sintel 0-01-23.png": the title, made safe for any file system, and the moment.
pub fn file_name(title: &str, pos: Ms) -> String {
    let mut name: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    name = name.trim().trim_end_matches('.').trim().to_owned();
    if name.chars().count() > 80 {
        name = name
            .chars()
            .take(80)
            .collect::<String>()
            .trim_end()
            .to_owned();
    }
    if name.is_empty() {
        name = "Glossplay".to_owned();
    }
    let s = pos.max(0) / 1000;
    format!("{name} {}-{:02}-{:02}.png", s / 3600, s / 60 % 60, s % 60)
}

/// `dir/name`, or "name (2)" and so on when that file exists already.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .expect("some number is free")
}

/// A rectangle of a window read back by OpenGL (rows bottom up), as top-down RGBA rows with
/// full alpha. `rect` is `[left, top, right, bottom]` in pixels from the top-left corner and
/// is clipped to the image.
pub fn crop(
    width: u32,
    height: u32,
    bottom_up: &[u8],
    rect: [u32; 4],
) -> Option<(u32, u32, Vec<u8>)> {
    let [left, top, right, bottom] = rect;
    let (right, bottom) = (right.min(width), bottom.min(height));
    if left >= right || top >= bottom || bottom_up.len() < (width * height * 4) as usize {
        return None;
    }
    let (w, h) = (right - left, bottom - top);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in top..bottom {
        let from_bottom = height - 1 - row;
        let start = ((from_bottom * width + left) * 4) as usize;
        out.extend_from_slice(&bottom_up[start..start + (w * 4) as usize]);
    }
    for alpha in out.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    Some((w, h, out))
}

pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer.write_image_data(rgba).map_err(std::io::Error::other)
}

/// The size of a PNG, from its header alone.
pub fn png_size(path: &Path) -> Option<(u32, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .ok()?;
    let info = reader.info();
    Some((info.width, info.height))
}

/// A PNG as 8-bit RGBA, whatever depth and colour type mpv wrote it in.
pub fn read_png(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    // A 4K frame with 16 bits a channel is over the default 64 MiB limit.
    let limits = png::Limits { bytes: 1 << 30 };
    let mut decoder = png::Decoder::new_with_limits(std::io::BufReader::new(file), limits);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or("the image is too large")?
    ];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[g, a]| [g, g, g, a])
            .collect(),
        png::ColorType::Indexed => return Err("an indexed PNG was not expanded".into()),
    };
    Ok((info.width, info.height, rgba))
}

/// Opens the file manager at `path`, with the file selected where the system can do that.
pub fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer reads its own command line: the path must be quoted after the comma.
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(drop)
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(drop)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let dir = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

/// A link that opens `url` at `secs`: YouTube takes `t=` in the query, other sites and
/// direct links to a video file the media fragment `#t=`.
pub fn link_at(url: &str, secs: i64) -> String {
    let secs = secs.max(0);
    let (base, _) = url.split_once('#').unwrap_or((url, ""));
    let host = host_of(base).unwrap_or_default().to_ascii_lowercase();
    let youtube = host == "youtu.be"
        || host == "youtube.com"
        || host.ends_with(".youtube.com")
        || host == "youtube-nocookie.com"
        || host.ends_with(".youtube-nocookie.com");
    if !youtube {
        return format!("{base}#t={secs}");
    }
    let (path, query) = base.split_once('?').unwrap_or((base, ""));
    let mut params: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("t=") && !p.starts_with("start="))
        .collect();
    let t = format!("t={secs}");
    params.push(&t);
    format!("{path}?{}", params.join("&"))
}

/// The host of a URL, without the user name, password or port.
pub fn host_of(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        host.split(':').next()?
    };
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_and_carry_the_moment() {
        assert_eq!(file_name("Sintel", 83_400), "Sintel 0-01-23.png");
        assert_eq!(file_name("a/b: c?*", 3_723_000), "a_b_ c__ 1-02-03.png");
        assert_eq!(file_name("  ...  ", 0), "Glossplay 0-00-00.png");
        let long = "x".repeat(200);
        assert_eq!(
            file_name(&long, 0).chars().count(),
            80 + " 0-00-00.png".len()
        );
    }

    #[test]
    fn an_existing_file_is_not_overwritten() {
        let dir = gp_core::scratch::TempDir::new("gp-frame-names");
        assert_eq!(
            unique_path(&dir, "a 0-00-01.png"),
            dir.join("a 0-00-01.png")
        );
        std::fs::write(dir.join("a 0-00-01.png"), b"x").unwrap();
        assert_eq!(
            unique_path(&dir, "a 0-00-01.png"),
            dir.join("a 0-00-01 (2).png")
        );
        std::fs::write(dir.join("a 0-00-01 (2).png"), b"x").unwrap();
        assert_eq!(
            unique_path(&dir, "a 0-00-01.png"),
            dir.join("a 0-00-01 (3).png")
        );
    }

    #[test]
    fn a_crop_turns_the_rows_over_and_makes_them_opaque() {
        // 3x2 image, bottom row first; each pixel's red is its index from the top-left.
        let (w, h) = (3, 2);
        let mut bottom_up = Vec::new();
        for row in (0..h).rev() {
            for col in 0..w {
                bottom_up.extend_from_slice(&[(row * w + col) as u8, 0, 0, 10]);
            }
        }
        let (cw, ch, px) = crop(w, h, &bottom_up, [1, 0, 3, 2]).unwrap();
        assert_eq!((cw, ch), (2, 2));
        let reds: Vec<u8> = px.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        assert_eq!(reds, [1, 2, 4, 5]);
        assert!(px.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        assert_eq!(
            crop(w, h, &bottom_up, [0, 0, 9, 9]).unwrap().0,
            3,
            "clipped"
        );
        assert!(crop(w, h, &bottom_up, [2, 0, 2, 2]).is_none(), "empty");
    }

    #[test]
    fn a_png_reads_back_as_rgba() {
        let dir = gp_core::scratch::TempDir::new("gp-frame-png");
        let path = dir.join("f.png");
        let px = [1, 2, 3, 255, 4, 5, 6, 255];
        write_png(&path, 2, 1, &px).unwrap();
        assert_eq!(read_png(&path).unwrap(), (2, 1, px.to_vec()));
    }

    #[test]
    fn links_open_at_the_moment() {
        assert_eq!(
            link_at("https://www.youtube.com/watch?v=abc&t=5s&list=x", 754),
            "https://www.youtube.com/watch?v=abc&list=x&t=754"
        );
        assert_eq!(
            link_at("https://youtu.be/abc", 3),
            "https://youtu.be/abc?t=3"
        );
        assert_eq!(
            link_at("https://example.test/v.mp4#t=9", 12),
            "https://example.test/v.mp4#t=12"
        );
    }

    #[test]
    fn hosts_lose_credentials_and_ports() {
        assert_eq!(host_of("https://u:p@cdn.test:8443/a?b"), Some("cdn.test"));
        assert_eq!(host_of("http://[::1]:8090/stream"), Some("::1"));
        assert_eq!(host_of("C:\\a.mkv"), None);
        assert_eq!(host_of("https:///x"), None);
    }
}
