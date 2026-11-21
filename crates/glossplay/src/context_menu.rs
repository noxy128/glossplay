//! The menu a right click on the picture opens: the usual player actions, each with its key,
//! the Subtitles menu of the control bar, the video picture settings and the frame tools.

use crate::app::App;
use crate::i18n::{Tr, t, tf, tk};
use crate::keys::{self, Command};
use crate::theme;
use crate::ui::{ControlsFit, SubtitlePicks};
use eframe::egui::{self, Id, Pos2, Rect, Response, RichText};

/// Speeds offered, as in the control bar.
const SPEEDS: [f64; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
/// `video-aspect-override` values: "-1" keeps the file's own.
const ASPECTS: [(&str, &str); 4] = [
    ("-1", ""),
    ("16:9", "16:9"),
    ("4:3", "4:3"),
    ("2.35:1", "2.35:1"),
];

pub fn id() -> Id {
    Id::unique("player-context-menu")
}

/// What was chosen in the menu during a frame, done once it is drawn.
pub struct Picks {
    command: Option<Command>,
    speed: Option<f64>,
    audio: Option<i64>,
    aspect: Option<&'static str>,
    rotate: bool,
    fill: Option<bool>,
    deinterlace: Option<bool>,
    reset_video: bool,
    save_frame: Option<bool>,
    copy_frame: bool,
    ocr_frame: bool,
    ocr_frame_translate: bool,
    copy_time: bool,
    copy_link: bool,
    paste: bool,
    on_top: bool,
    settings: bool,
    help: bool,
    subs: SubtitlePicks,
    /// Where the submenus and some items are, for the self-test that clicks them.
    pub places: Vec<(&'static str, Rect)>,
}

/// The picture settings of mpv the menu changes, read while it is open.
struct Picture {
    aspect: String,
    rotate: i64,
    fill: bool,
    deinterlace: bool,
}

impl Picture {
    fn read(mpv: &gp_media::mpv::Mpv) -> Picture {
        Picture {
            aspect: mpv.get_string("video-aspect-override").unwrap_or_default(),
            rotate: mpv.get_i64("video-rotate").unwrap_or(0),
            fill: mpv.get_f64("panscan").unwrap_or(0.0) > 0.5,
            deinterlace: mpv.get_string("deinterlace").as_deref() == Some("yes"),
        }
    }

    fn changed(&self) -> bool {
        !aspect_is(&self.aspect, "-1") || self.rotate != 0 || self.fill || self.deinterlace
    }
}

/// mpv writes the override back as a number ("1.777778") or as "no"/"-1".
fn aspect_is(current: &str, value: &str) -> bool {
    let ratio = |s: &str| -> Option<f64> {
        match s.split_once(':') {
            Some((a, b)) => Some(a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?),
            None => s.trim().parse().ok(),
        }
    };
    match (ratio(current), ratio(value)) {
        (Some(a), Some(b)) if b <= 0.0 => a <= 0.0,
        (Some(a), Some(b)) => (a - b).abs() < 0.01,
        _ => current == value,
    }
}

/// Puts the picture back as the file has it; done for every file opened.
pub fn reset_picture(mpv: &gp_media::mpv::Mpv) {
    mpv.set("video-aspect-override", "-1");
    mpv.set("video-rotate", "0");
    mpv.set_f64("panscan", 0.0);
    mpv.set("deinterlace", "no");
}

/// A menu item with the key that does the same at its right.
fn item(
    ui: &mut egui::Ui,
    enabled: bool,
    selected: Option<bool>,
    text: &str,
    key: Option<Command>,
) -> Response {
    let mut button = match selected {
        Some(on) => egui::Button::selectable(on, text),
        None => egui::Button::new(text),
    };
    if let Some(k) = key.and_then(|c| keys::keys_of(c).into_iter().next()) {
        button = button.shortcut_text(RichText::new(k).color(theme::muted()));
    }
    ui.add_enabled(enabled, button)
}

/// Notes where an item is and tells whether it was clicked.
fn placed(places: &mut Vec<(&'static str, Rect)>, name: &'static str, r: Response) -> bool {
    places.push((name, r.rect));
    r.clicked()
}

impl App {
    /// Opens the menu at `at`; the self-test calls this too.
    pub fn open_context_menu(&mut self, ctx: &egui::Context, at: Pos2) {
        self.menu_at = Some(at);
        egui::Popup::open_id(ctx, id());
    }

    /// Draws the menu while it is open. `picture` is the response of the video area, whose
    /// right click opens it and whose left click closes it.
    pub fn context_menu(&mut self, ui: &egui::Ui, picture: &Response) {
        let ctx = ui.ctx().clone();
        if let Some(at) = self.menu_request.take() {
            self.open_context_menu(&ctx, at);
        } else if picture.clicked() {
            egui::Popup::close_id(&ctx, id());
        }
        let Some(at) = self.menu_at else { return };
        let mut picks = Picks {
            command: None,
            speed: None,
            audio: None,
            aspect: None,
            rotate: false,
            fill: None,
            deinterlace: None,
            reset_video: false,
            save_frame: None,
            copy_frame: false,
            copy_time: false,
            copy_link: false,
            paste: false,
            on_top: false,
            settings: false,
            help: false,
            subs: SubtitlePicks::new(&self.cfg),
            places: Vec::new(),
        };
        let shown = egui::Popup::menu(picture)
            .id(id())
            .open_memory(None)
            .at_position(at)
            .show(|ui| self.context_menu_ui(ui, &mut picks));
        if shown.is_none() {
            self.menu_at = None;
        }
        self.menu_rects = std::mem::take(&mut picks.places);
        self.apply_menu(picks, &ctx);
    }

    fn context_menu_ui(&self, ui: &mut egui::Ui, p: &mut Picks) {
        ui.set_min_width(270.0);
        let has_subs = self
            .session
            .as_ref()
            .is_some_and(|s| s.primary_track().is_some());
        let mpv = &self.player.mpv;

        let play = if self.snap.paused {
            t(Tr::MenuPlay)
        } else {
            t(Tr::MenuPause)
        };
        if placed(
            &mut p.places,
            "play",
            item(ui, true, None, play, Some(Command::PlayPause)),
        ) {
            p.command = Some(Command::PlayPause);
        }
        let speed = self.snap.speed;
        let r = ui.menu_button(
            format!(
                "{} · {}×",
                t(Tr::MenuSpeed),
                crate::i18n::decimal(format!("{speed}"))
            ),
            |ui| {
                for s in SPEEDS {
                    let label = crate::i18n::decimal(format!("{s}×"));
                    if ui
                        .add(egui::Button::selectable((speed - s).abs() < 1e-6, label))
                        .clicked()
                    {
                        p.speed = Some(s);
                    }
                }
            },
        );
        p.places.push(("speed", r.response.rect));
        if item(
            ui,
            has_subs,
            None,
            t(Tr::MenuRepeatLine),
            Some(Command::RepeatLine),
        )
        .clicked()
        {
            p.command = Some(Command::RepeatLine);
        }
        if item(
            ui,
            true,
            Some(self.trainer.autopause),
            t(Tr::AutoPause),
            Some(Command::AutoPause),
        )
        .clicked()
        {
            p.command = Some(Command::AutoPause);
        }
        ui.separator();

        if item(ui, true, None, t(Tr::OpenVideo), Some(Command::OpenFile)).clicked() {
            p.command = Some(Command::OpenFile);
        }
        let paste = egui::Button::new(t(Tr::MenuOpenClipboard))
            .shortcut_text(RichText::new(tk(Tr::KeyCtrlV)).color(theme::muted()));
        if ui.add(paste).clicked() {
            p.paste = true;
        }
        ui.separator();

        if self.audio_tracks.is_empty() {
            ui.add_enabled(false, egui::Button::new(t(Tr::MenuAudioTrack)));
        } else {
            let r = ui.menu_button(t(Tr::MenuAudioTrack), |ui| {
                for a in &self.audio_tracks {
                    if ui
                        .add(egui::Button::selectable(a.selected, a.label()))
                        .clicked()
                    {
                        p.audio = Some(a.id);
                    }
                }
            });
            p.places.push(("audio", r.response.rect));
        }
        let r = ui.menu_button(t(Tr::SubtitlesMenu), |ui| {
            self.subtitle_menu_ui(ui, &ControlsFit::all(), &mut p.subs);
        });
        p.places.push(("subtitles", r.response.rect));
        let r = ui.menu_button(t(Tr::MenuVideo), |ui| {
            let now = Picture::read(mpv);
            ui.label(
                RichText::new(t(Tr::MenuAspect))
                    .color(theme::muted())
                    .small(),
            );
            for (value, name) in ASPECTS {
                let name = if name.is_empty() {
                    t(Tr::MenuAspectAuto)
                } else {
                    name
                };
                let r = ui.add(egui::Button::selectable(
                    aspect_is(&now.aspect, value),
                    name,
                ));
                if placed(&mut p.places, value, r) {
                    p.aspect = Some(value);
                }
            }
            ui.separator();
            let rotate = if now.rotate % 360 == 0 {
                t(Tr::MenuRotate).to_owned()
            } else {
                format!("{} ({}°)", t(Tr::MenuRotate), now.rotate % 360)
            };
            if placed(&mut p.places, "rotate", ui.button(rotate)) {
                p.rotate = true;
            }
            let r = ui.add(egui::Button::selectable(now.fill, t(Tr::MenuFill)));
            if placed(&mut p.places, "fill", r) {
                p.fill = Some(!now.fill);
            }
            let r = ui.add(egui::Button::selectable(
                now.deinterlace,
                t(Tr::MenuDeinterlace),
            ));
            if placed(&mut p.places, "deinterlace", r) {
                p.deinterlace = Some(!now.deinterlace);
            }
            ui.separator();
            if ui
                .add_enabled(now.changed(), egui::Button::new(t(Tr::MenuVideoReset)))
                .clicked()
            {
                p.reset_video = true;
            }
        });
        p.places.push(("video", r.response.rect));
        ui.separator();

        let r = ui.menu_button(t(Tr::MenuFrame), |ui| {
            if placed(&mut p.places, "save_frame", ui.button(t(Tr::MenuSaveFrame))) {
                p.save_frame = Some(false);
            }
            let r = ui.button(t(Tr::MenuSaveFrameSubs));
            if placed(&mut p.places, "save_frame_subtitles", r) {
                p.save_frame = Some(true);
            }
            if ui.button(t(Tr::MenuCopyFrame)).clicked() {
                p.copy_frame = true;
            }
            ui.separator();
            let ocr_enabled = self.config.ocr.enabled;
            if ui.add_enabled(ocr_enabled, egui::Button::new(t(Tr::MenuOcrFrame))).clicked() {
                p.ocr_frame = true;
            }
            if ui.add_enabled(ocr_enabled, egui::Button::new(t(Tr::MenuOcrFrameTranslate))).clicked() {
                p.ocr_frame_translate = true;
            }
        });
        p.places.push(("frame", r.response.rect));
        let clock = theme::clock(self.snap.pos);
        if placed(
            &mut p.places,
            "copy_time",
            ui.button(tf(Tr::MenuCopyTime, &[&clock])),
        ) {
            p.copy_time = true;
        }
        if self.moment_link().is_some() && ui.button(t(Tr::MenuCopyLink)).clicked() {
            p.copy_link = true;
        }
        ui.separator();

        if item(
            ui,
            true,
            Some(self.fullscreen),
            t(Tr::MenuFullscreen),
            Some(Command::Fullscreen),
        )
        .clicked()
        {
            p.command = Some(Command::Fullscreen);
        }
        if item(ui, true, Some(self.on_top), t(Tr::MenuOnTop), None).clicked() {
            p.on_top = true;
        }
        let r = item(
            ui,
            true,
            Some(self.info_open),
            t(Tr::InfoWindowTitle),
            Some(Command::VideoInfo),
        );
        if placed(&mut p.places, "video_info", r) {
            p.command = Some(Command::VideoInfo);
        }
        ui.separator();
        if ui.button(t(Tr::SettingsMenu)).clicked() {
            p.settings = true;
        }
        if ui.button(t(Tr::HelpMenu)).clicked() {
            p.help = true;
        }
    }

    fn apply_menu(&mut self, p: Picks, ctx: &egui::Context) {
        let mpv = self.player.mpv.clone();
        if let Some(c) = p.command {
            // A self-test must not open a system dialog or take over the screen.
            let skip =
                self.selftest.is_some() && matches!(c, Command::OpenFile | Command::Fullscreen);
            if !skip {
                self.run_command(c, ctx);
            }
        }
        if let Some(s) = p.speed {
            mpv.set_f64("speed", s);
        }
        if let Some(id) = p.audio {
            mpv.set("aid", &id.to_string());
            for a in &mut self.audio_tracks {
                a.selected = a.id == id;
            }
        }
        if let Some(a) = p.aspect {
            mpv.set("video-aspect-override", a);
        }
        if p.rotate {
            let now = mpv.get_i64("video-rotate").unwrap_or(0);
            mpv.set("video-rotate", &((now + 90) % 360).to_string());
        }
        if let Some(fill) = p.fill {
            mpv.set_f64("panscan", if fill { 1.0 } else { 0.0 });
        }
        if let Some(on) = p.deinterlace {
            mpv.set("deinterlace", if on { "yes" } else { "no" });
        }
        if p.reset_video {
            reset_picture(&mpv);
        }
        if let Some(subtitles) = p.save_frame
            && let Err(e) = self.save_frame(subtitles, None)
        {
            self.toast(tf(Tr::FrameNotSaved, &[&e]));
        }
        if p.copy_frame {
            self.copy_frame();
        }
        if p.ocr_frame {
            self.ocr_current_frame(false);
        }
        if p.ocr_frame_translate {
            self.ocr_current_frame(true);
        }
        if p.copy_time {
            let clock = theme::clock(self.snap.pos);
            self.copy_text(ctx, clock.clone());
            self.toast_for(tf(Tr::Copied, &[&clock]), std::time::Duration::from_secs(2));
        }
        if p.copy_link
            && let Some(link) = self.moment_link()
        {
            self.copy_text(ctx, link);
            self.toast_for(t(Tr::LinkCopied), std::time::Duration::from_secs(2));
        }
        if p.paste {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
        if p.on_top {
            self.on_top = !self.on_top;
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if self.on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
        }
        if p.settings {
            self.open_settings(None);
        }
        if p.help {
            self.help_open = true;
        }
        self.apply_subtitle_picks(p.subs);
    }

    /// A self-test leaves the user's clipboard alone.
    pub fn copy_text(&self, ctx: &egui::Context, text: String) {
        if !self.quiet {
            ctx.copy_text(text);
        }
    }

    /// A link to the moment on screen, for a video opened by its address on the web.
    pub fn moment_link(&self) -> Option<String> {
        let s = self.session.as_ref()?;
        let web = s.media.starts_with("http://") || s.media.starts_with("https://");
        if !web || self.torrserver_stream(&s.media).is_some() {
            return None;
        }
        Some(crate::frame::link_at(&s.media, self.snap.pos / 1000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aspect_is_matched_however_mpv_writes_it() {
        assert!(aspect_is("-1", "-1"));
        assert!(aspect_is("-1.000000", "-1"));
        assert!(aspect_is("1.777778", "16:9"));
        assert!(aspect_is("16:9", "16:9"));
        assert!(!aspect_is("1.333333", "16:9"));
        assert!(aspect_is("2.350000", "2.35:1"));
        assert!(!aspect_is("1.777778", "-1"));
    }
}
