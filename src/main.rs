#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::AnimationDecoder;
use serde::{Deserialize, Serialize};
use slint::{Image as SlintImage, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};
const APP_NAME: &str = "Simple Media Overlay";
const CONFIG_DIR: &str = "simple-media-overlay";

enum Playback {
    Idle,
    Frames { frames: Vec<(SlintImage, Duration)>, index: usize },
    #[cfg(feature = "video")]
    Video(video::VideoStream),
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Settings {
    last_file: Option<PathBuf>,
    icon_file: Option<PathBuf>,
    overlay_x: Option<i32>,
    overlay_y: Option<i32>,
    overlay_w: Option<u32>,
    overlay_h: Option<u32>,
    #[serde(default)]
    muted: bool,
}

impl Settings {
    fn file_path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(std::env::temp_dir);
        base.join(CONFIG_DIR).join("settings.json")
    }

    fn load() -> Self {
        std::fs::read_to_string(Self::file_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let path = Self::file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

fn main() {
    let overlay = OverlayWindow::new().expect("failed to create overlay window");
    let home = HomeWindow::new().expect("failed to create home window");

    let playback: Rc<RefCell<Playback>> = Rc::new(RefCell::new(Playback::Idle));
    let timer: Rc<Timer> = Rc::new(Timer::default());
    let settings: Rc<RefCell<Settings>> = Rc::new(RefCell::new(Settings::load()));
    let audio: Rc<RefCell<audio::AudioPlayer>> =
        Rc::new(RefCell::new(audio::AudioPlayer::new(settings.borrow().muted)));

    home.set_muted(settings.borrow().muted);
    home.set_has_audio(false);

    if let Some(icon_path) = settings.borrow().icon_file.clone() {
        if let Ok(img) = image::open(&icon_path) {
            home.set_icon_image(to_slint_image(&img.to_rgba8()));
        }
    }

    {
        let s = settings.borrow();
        let window = overlay.window();
        let scale = window.scale_factor();

        if let (Some(w), Some(h)) = (s.overlay_w, s.overlay_h) {
            window.set_size(slint::PhysicalSize::new(w, h));
            home.set_width_text(((w as f32 / scale).round() as i32).to_string().into());
            home.set_height_text(((h as f32 / scale).round() as i32).to_string().into());
        }
        if let (Some(x), Some(y)) = (s.overlay_x, s.overlay_y) {
            window.set_position(slint::PhysicalPosition::new(x, y));
        }
    }

    let last_file = settings.borrow().last_file.clone();
    if let Some(path) = last_file {
        if path.exists() {
            open_path(
                &path,
                &overlay.as_weak(),
                &home.as_weak(),
                &playback,
                &timer,
                &settings,
                &audio,
            );
        } else {
            home.set_status_text(format!("{} (moved/deleted)", path.display()).into());
        }
    }

    home.show().expect("failed to show home window");

    {
        let overlay_weak = overlay.as_weak();
        let home_weak = home.as_weak();
        let playback = playback.clone();
        let timer = timer.clone();
        let settings = settings.clone();
        let audio = audio.clone();

        home.on_open_file(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Media", &supported_extensions())
                .add_filter("Images & GIFs", &IMAGE_EXTENSIONS)
                .add_filter("Audio", &AUDIO_EXTENSIONS)
                .pick_file()
            {
                open_path(&path, &overlay_weak, &home_weak, &playback, &timer, &settings, &audio);
            }
        });
    }

    {
        let home_weak = home.as_weak();
        let settings = settings.clone();

        home.on_pick_icon(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Icon image", &["png", "ico", "jpg", "jpeg", "bmp"])
                .pick_file()
            {
                if let Ok(img) = image::open(&path) {
                    let icon_image = to_slint_image(&img.to_rgba8());
                    if let Some(home) = home_weak.upgrade() {
                        home.set_icon_image(icon_image);
                    }
                    settings.borrow_mut().icon_file = Some(path);
                    settings.borrow().save();
                } else {
                    eprintln!("Could not load icon {path:?}");
                }
            }
        });
    }

    {
        let home_weak = home.as_weak();
        let settings = settings.clone();
        let audio = audio.clone();

        home.on_toggle_mute(move || {
            let muted = !audio.borrow().muted();
            audio.borrow_mut().set_muted(muted);
            if let Some(home) = home_weak.upgrade() {
                home.set_muted(muted);
            }
            let mut s = settings.borrow_mut();
            s.muted = muted;
            s.save();
        });
    }

    {
        let overlay_weak = overlay.as_weak();
        let home_weak = home.as_weak();
        let settings = settings.clone();

        home.on_set_size(move |w_text, h_text| {
            let (Ok(w), Ok(h)) = (w_text.trim().parse::<f32>(), h_text.trim().parse::<f32>())
            else {
                return;
            };
            if w < 80.0 || h < 60.0 {
                return;
            }
            let Some(overlay) = overlay_weak.upgrade() else { return };
            let window = overlay.window();
            let scale = window.scale_factor();
            window.set_size(slint::PhysicalSize::new((w * scale) as u32, (h * scale) as u32));
            let size = window.size();
            if let Some(home) = home_weak.upgrade() {
                home.set_width_text(((size.width as f32 / scale).round() as i32).to_string().into());
                home.set_height_text(((size.height as f32 / scale).round() as i32).to_string().into());
            }

            let mut s = settings.borrow_mut();
            s.overlay_w = Some(size.width);
            s.overlay_h = Some(size.height);
            s.save();
        });
    }

    {
        let home_weak = home.as_weak();
        home.on_hide_to_tray(move || {
            if let Some(home) = home_weak.upgrade() {
                let _ = home.window().hide();
            }
        });
    }

    home.on_quit_app(|| {
        let _ = slint::quit_event_loop();
    });

    {
        let overlay_weak = overlay.as_weak();
        overlay.on_close_requested(move || {
            if let Some(overlay) = overlay_weak.upgrade() {
                let _ = overlay.window().hide();
            }
        });
    }

    {
        let overlay_weak = overlay.as_weak();
        overlay.on_drag_window(move |dx, dy| {
            let Some(overlay) = overlay_weak.upgrade() else { return };
            let window = overlay.window();
            let scale = window.scale_factor();
            let cur = window.position();
            window.set_position(slint::PhysicalPosition::new(
                cur.x + (dx * scale) as i32,
                cur.y + (dy * scale) as i32,
            ));
        });
    }

    {
        let overlay_weak = overlay.as_weak();
        let settings = settings.clone();
        overlay.on_drag_window_end(move || {
            let Some(overlay) = overlay_weak.upgrade() else { return };
            let pos = overlay.window().position();
            let mut s = settings.borrow_mut();
            s.overlay_x = Some(pos.x);
            s.overlay_y = Some(pos.y);
            s.save();
        });
    }

    {
        let overlay_weak = overlay.as_weak();
        overlay.on_resize_window(move |dx, dy| {
            let Some(overlay) = overlay_weak.upgrade() else { return };
            let window = overlay.window();
            let scale = window.scale_factor();
            let cur = window.size();
            let new_w = ((cur.width as f32 + dx * scale).max(80.0 * scale)) as u32;
            let new_h = ((cur.height as f32 + dy * scale).max(60.0 * scale)) as u32;
            window.set_size(slint::PhysicalSize::new(new_w, new_h));
        });
    }

    {
        let overlay_weak = overlay.as_weak();
        let home_weak = home.as_weak();
        let settings = settings.clone();

        overlay.on_resize_window_end(move || {
            let Some(overlay) = overlay_weak.upgrade() else { return };
            let window = overlay.window();
            let size = window.size();
            let scale = window.scale_factor();
            if let Some(home) = home_weak.upgrade() {
                home.set_width_text(((size.width as f32 / scale).round() as i32).to_string().into());
                home.set_height_text(((size.height as f32 / scale).round() as i32).to_string().into());
            }

            let mut s = settings.borrow_mut();
            s.overlay_w = Some(size.width);
            s.overlay_h = Some(size.height);
            s.save();
        });
    }

    let _tray_guard = tray_support::setup(&home, &overlay);

    let audio_loop_timer = Timer::default();
    {
        let audio = audio.clone();
        audio_loop_timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            audio.borrow_mut().poll_loop();
        });
    }

    slint::run_event_loop().expect("event loop failed");
    drop(audio_loop_timer);
}

const IMAGE_EXTENSIONS: [&str; 7] = ["gif", "png", "jpg", "jpeg", "webp", "bmp", "ico"];
const AUDIO_EXTENSIONS: [&str; 6] = ["mp3", "m4a", "aac", "wav", "flac", "ogg"];
#[cfg(feature = "video")]
const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "m4v", "mkv", "mov", "webm", "avi"];

fn supported_extensions() -> Vec<&'static str> {
    let mut exts: Vec<&'static str> = IMAGE_EXTENSIONS.to_vec();
    #[cfg(feature = "video")]
    exts.extend_from_slice(&VIDEO_EXTENSIONS);
    exts.extend_from_slice(&AUDIO_EXTENSIONS);
    exts
}

#[cfg(feature = "video")]
fn is_video_ext(ext: &str) -> bool {
    VIDEO_EXTENSIONS.contains(&ext)
}

#[cfg(not(feature = "video"))]
fn is_video_ext(_ext: &str) -> bool {
    false
}

fn is_audio_ext(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext)
}

fn open_path(
    path: &Path,
    overlay_weak: &slint::Weak<OverlayWindow>,
    home_weak: &slint::Weak<HomeWindow>,
    playback: &Rc<RefCell<Playback>>,
    timer: &Rc<Timer>,
    settings: &Rc<RefCell<Settings>>,
    audio: &Rc<RefCell<audio::AudioPlayer>>,
) {
    timer.stop();
    *playback.borrow_mut() = Playback::Idle;
    audio.borrow_mut().stop();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase();

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let has_audio = if is_video_ext(&ext) || is_audio_ext(&ext) {
        audio.borrow_mut().load(path)
    } else {
        false
    };

    if let Some(home) = home_weak.upgrade() {
        home.set_has_audio(has_audio);
    }

    let mut ok = true;

    if is_video_ext(&ext) {
        #[cfg(feature = "video")]
        match video::VideoStream::open(path) {
            Some(stream) => {
                if let Some(overlay) = overlay_weak.upgrade() {
                    overlay.set_current_image(SlintImage::default());
                    overlay.set_has_image(false);
                    overlay.set_caption(name.clone().into());
                    let _ = overlay.show();
                }
                *playback.borrow_mut() = Playback::Video(stream);
                start_video_timer(overlay_weak.clone(), playback.clone(), timer.clone(), audio.clone());
                if has_audio {
                    audio.borrow_mut().play(false);
                }
            }
            None => {
                ok = false;
                if !video::ffmpeg_available() {
                    if let Some(home) = home_weak.upgrade() {
                        home.set_status_text(
                            "Video needs ffmpeg on PATH (winget install Gyan.FFmpeg)".into(),
                        );
                        home.set_has_audio(false);
                    }
                    return;
                }
            }
        }
    } else if is_audio_ext(&ext) {
        if has_audio {
            if let Some(overlay) = overlay_weak.upgrade() {
                overlay.set_current_image(SlintImage::default());
                overlay.set_has_image(false);
                overlay.set_caption(name.clone().into());
                let _ = overlay.show();
            }
            audio.borrow_mut().play(true);
        } else {
            ok = false;
        }
    } else {
        let frames = if ext == "gif" { load_gif(path) } else { load_static_image(path) };

        match frames {
            Some(frames) if !frames.is_empty() => {
                if let Some(overlay) = overlay_weak.upgrade() {
                    overlay.set_current_image(frames[0].0.clone());
                    overlay.set_has_image(true);
                    overlay.set_caption("".into());
                    let _ = overlay.show();
                }
                let multi = frames.len() > 1;
                *playback.borrow_mut() = Playback::Frames { frames, index: 0 };
                if multi {
                    schedule_next_frame(overlay_weak.clone(), playback.clone(), timer.clone());
                }
            }
            _ => ok = false,
        }
    }

    if !ok {
        eprintln!("Could not load {path:?}");
        if let Some(home) = home_weak.upgrade() {
            home.set_status_text(format!("Couldn't load {}", path.display()).into());
            home.set_has_audio(false);
        }
        return;
    }

    if let Some(home) = home_weak.upgrade() {
        home.set_status_text(name.into());
    }

    let mut s = settings.borrow_mut();
    s.last_file = Some(path.to_path_buf());
    s.save();
}

fn load_static_image(path: &Path) -> Option<Vec<(SlintImage, Duration)>> {
    let img = image::open(path).ok()?.to_rgba8();
    Some(vec![(to_slint_image(&img), Duration::ZERO)])
}

fn load_gif(path: &Path) -> Option<Vec<(SlintImage, Duration)>> {
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let decoder = GifDecoder::new(file).ok()?;
    let frames = decoder.into_frames().collect_frames().ok()?;

    Some(
        frames
            .iter()
            .map(|frame| {
                let (numer, denom) = frame.delay().numer_denom_ms();
                let ms = if denom == 0 { 100 } else { numer / denom };
                let delay = Duration::from_millis(ms.max(20) as u64);
                (to_slint_image(frame.buffer()), delay)
            })
            .collect(),
    )
}

fn to_slint_image(rgba: &image::RgbaImage) -> SlintImage {
    let (width, height) = rgba.dimensions();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(rgba.as_raw());
    SlintImage::from_rgba8(buffer)
}

fn schedule_next_frame(
    overlay_weak: slint::Weak<OverlayWindow>,
    playback: Rc<RefCell<Playback>>,
    timer: Rc<Timer>,
) {
    let delay = match &*playback.borrow() {
        Playback::Frames { frames, index } if !frames.is_empty() => frames[*index].1,
        _ => return,
    };

    let playback_for_closure = playback.clone();
    let overlay_weak_for_closure = overlay_weak.clone();
    let timer_for_closure = timer.clone();

    timer.start(TimerMode::SingleShot, delay, move || {
        let next_frame = match &mut *playback_for_closure.borrow_mut() {
            Playback::Frames { frames, index } if !frames.is_empty() => {
                *index = (*index + 1) % frames.len();
                Some(frames[*index].0.clone())
            }
            _ => None,
        };

        if let (Some(frame), Some(overlay)) = (next_frame, overlay_weak_for_closure.upgrade()) {
            overlay.set_current_image(frame);
        }

        schedule_next_frame(
            overlay_weak_for_closure.clone(),
            playback_for_closure.clone(),
            timer_for_closure.clone(),
        );
    });
}

#[cfg(feature = "video")]
fn start_video_timer(
    overlay_weak: slint::Weak<OverlayWindow>,
    playback: Rc<RefCell<Playback>>,
    timer: Rc<Timer>,
    audio: Rc<RefCell<audio::AudioPlayer>>,
) {
    let delay = match &*playback.borrow() {
        Playback::Video(stream) => stream.frame_delay,
        _ => return,
    };

    timer.start(TimerMode::Repeated, delay, move || loop {
        let msg = match &*playback.borrow() {
            Playback::Video(stream) => stream.rx.try_recv().ok(),
            _ => None,
        };

        match msg {
            Some(video::VideoMsg::LoopPoint) => {
                audio.borrow_mut().restart();
                continue;
            }
            Some(video::VideoMsg::Frame { width, height, data }) => {
                if let Some(overlay) = overlay_weak.upgrade() {
                    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
                    buffer.make_mut_bytes().copy_from_slice(&data);
                    overlay.set_current_image(SlintImage::from_rgba8(buffer));
                    overlay.set_has_image(true);
                }
                break;
            }
            None => break,
        }
    });
}

#[cfg(not(feature = "video"))]
#[allow(dead_code)]
fn start_video_timer(
    _overlay_weak: slint::Weak<OverlayWindow>,
    _playback: Rc<RefCell<Playback>>,
    _timer: Rc<Timer>,
    _audio: Rc<RefCell<audio::AudioPlayer>>,
) {
}

mod audio {
    use rodio::buffer::SamplesBuffer;
    use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
    use std::path::Path;
    const MAX_SAMPLES: usize = 48_000 * 2 * 600;

    struct Track {
        channels: u16,
        rate: u32,
        samples: Vec<i16>,
    }

    pub struct AudioPlayer {
        _stream: Option<OutputStream>,
        handle: Option<OutputStreamHandle>,
        sink: Option<Sink>,
        track: Option<Track>,
        muted: bool,
        looping: bool,
    }

    impl AudioPlayer {
        pub fn new(muted: bool) -> Self {
            let (stream, handle) = match OutputStream::try_default() {
                Ok((stream, handle)) => (Some(stream), Some(handle)),
                Err(err) => {
                    eprintln!("No audio output device: {err}");
                    (None, None)
                }
            };
            Self { _stream: stream, handle, sink: None, track: None, muted, looping: false }
        }

        pub fn load(&mut self, path: &Path) -> bool {
            self.stop();
            self.track = None;

            if self.handle.is_none() {
                return false;
            }

            let Ok(file) = std::fs::File::open(path) else { return false };
            let Ok(decoder) = rodio::Decoder::new(std::io::BufReader::new(file)) else {
                return false;
            };

            let channels = decoder.channels();
            let rate = decoder.sample_rate();
            if channels == 0 || rate == 0 {
                return false;
            }

            let samples: Vec<i16> = decoder.take(MAX_SAMPLES).collect();
            if samples.is_empty() {
                return false;
            }

            self.track = Some(Track { channels, rate, samples });
            true
        }

        pub fn play(&mut self, looping: bool) {
            self.looping = looping;
            self.restart();
        }

        pub fn restart(&mut self) {
            self.stop();

            let (Some(handle), Some(track)) = (self.handle.as_ref(), self.track.as_ref()) else {
                return;
            };
            let Ok(sink) = Sink::try_new(handle) else { return };

            let buffer = SamplesBuffer::new(track.channels, track.rate, track.samples.clone());
            sink.append(buffer);
            sink.set_volume(if self.muted { 0.0 } else { 1.0 });
            sink.play();
            self.sink = Some(sink);
        }

        pub fn stop(&mut self) {
            if let Some(sink) = self.sink.take() {
                sink.stop();
            }
        }

        pub fn poll_loop(&mut self) {
            if !self.looping {
                return;
            }
            let drained = self.sink.as_ref().map(|s| s.empty()).unwrap_or(false);
            if drained {
                self.restart();
            }
        }

        pub fn set_muted(&mut self, muted: bool) {
            self.muted = muted;
            if let Some(sink) = self.sink.as_ref() {
                sink.set_volume(if muted { 0.0 } else { 1.0 });
            }
        }

        pub fn muted(&self) -> bool {
            self.muted
        }
    }
}

#[cfg(feature = "video")]
mod video {
    use std::io::{BufReader, Read};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
    use std::sync::Arc;
    use std::time::Duration;

    const MAX_EDGE: u32 = 1280;

    pub enum VideoMsg {
        Frame { width: u32, height: u32, data: Vec<u8> },
        LoopPoint,
    }

    pub struct VideoStream {
        pub rx: Receiver<VideoMsg>,
        pub frame_delay: Duration,
        cancel: Arc<AtomicBool>,
    }

    impl Drop for VideoStream {
        fn drop(&mut self) {
            self.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn command(program: &str) -> Command {
        let mut cmd = Command::new(program);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }

    pub fn ffmpeg_available() -> bool {
        command("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    struct Info {
        width: u32,
        height: u32,
        delay: Duration,
    }

    fn probe(path: &Path) -> Option<Info> {
        let output = command("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,r_frame_rate",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.lines().next()?.trim();
        let mut fields = line.split(',');

        let src_w: u32 = fields.next()?.trim().parse().ok()?;
        let src_h: u32 = fields.next()?.trim().parse().ok()?;
        let rate = fields.next().unwrap_or("").trim();
        let (num, den) = rate.split_once('/').unwrap_or((rate, "1"));
        let num: f64 = num.parse().unwrap_or(0.0);
        let den: f64 = den.parse().unwrap_or(1.0);

        let delay_ms = if num > 0.0 && den > 0.0 {
            ((1000.0 * den / num).round() as u64).clamp(8, 1000)
        } else {
            33
        };

        let (width, height) = target_size(src_w, src_h);
        Some(Info { width, height, delay: Duration::from_millis(delay_ms) })
    }

    impl VideoStream {
        pub fn open(path: &Path) -> Option<Self> {
            let info = probe(path)?;
            if info.width == 0 || info.height == 0 {
                return None;
            }

            let (tx, rx) = sync_channel(4);
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let worker_path: PathBuf = path.to_path_buf();
            let (width, height) = (info.width, info.height);

            std::thread::spawn(move || {
                decode_loop(worker_path, width, height, tx, worker_cancel)
            });

            Some(Self { rx, frame_delay: info.delay, cancel })
        }
    }

    fn decode_loop(
        path: PathBuf,
        width: u32,
        height: u32,
        tx: SyncSender<VideoMsg>,
        cancel: Arc<AtomicBool>,
    ) {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if !decode_once(&path, width, height, &tx, &cancel) {
                return;
            }
            if cancel.load(Ordering::Relaxed) || tx.send(VideoMsg::LoopPoint).is_err() {
                return;
            }
        }
    }

    fn decode_once(
        path: &Path,
        width: u32,
        height: u32,
        tx: &SyncSender<VideoMsg>,
        cancel: &AtomicBool,
    ) -> bool {
        let scale = format!("scale={width}:{height}");
        let Ok(mut child) = command("ffmpeg")
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(path)
            .args(["-an", "-vf", &scale, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
        else {
            return false;
        };

        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        };

        let frame_bytes = width as usize * height as usize * 4;
        let mut reader = BufReader::new(stdout);
        let mut buffer = vec![0u8; frame_bytes];
        let mut produced = false;

        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            if reader.read_exact(&mut buffer).is_err() {
                break;
            }
            produced = true;
            if tx.send(VideoMsg::Frame { width, height, data: buffer.clone() }).is_err() {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        produced
    }

    fn target_size(width: u32, height: u32) -> (u32, u32) {
        if width == 0 || height == 0 {
            return (0, 0);
        }
        let long_edge = width.max(height);
        if long_edge <= MAX_EDGE {
            return (width & !1, height & !1);
        }
        let scale = MAX_EDGE as f32 / long_edge as f32;
        let w = ((width as f32 * scale) as u32).max(2) & !1;
        let h = ((height as f32 * scale) as u32).max(2) & !1;
        (w, h)
    }
}

mod tray_support {
    use super::{HomeWindow, OverlayWindow, APP_NAME};
    use slint::{ComponentHandle, Timer, TimerMode};
    use std::time::Duration;
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem},
        TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    pub struct TrayGuard {
        _tray_icon: TrayIcon,
        _poll_timer: Timer,
    }

    pub fn setup(home: &HomeWindow, overlay: &OverlayWindow) -> TrayGuard {
        #[cfg(target_os = "windows")]
        {
            use i_slint_backend_winit::WinitWindowAccessor;
            use winit::platform::windows::WindowExtWindows;
            overlay.window().with_winit_window(|winit_window| {
                winit_window.set_skip_taskbar(true);
            });
        }
        #[cfg(not(target_os = "windows"))]
        let _ = overlay;

        let size = 32u32;
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        for px in rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&[70, 140, 230, 255]);
        }
        let icon = tray_icon::Icon::from_rgba(rgba, size, size).expect("bad tray icon data");

        let menu = Menu::new();
        let show_item = MenuItem::new("Show Home", true, None);
        let hide_item = MenuItem::new("Hide Home", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        menu.append(&show_item).ok();
        menu.append(&hide_item).ok();
        menu.append(&quit_item).ok();
        let show_id = show_item.id().clone();
        let hide_id = hide_item.id().clone();
        let quit_id = quit_item.id().clone();

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(APP_NAME)
            .with_icon(icon)
            .build()
            .expect("failed to create tray icon");

        let home_weak = home.as_weak();
        let poll_timer = Timer::default();
        poll_timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    ..
                } = event
                {
                    if let Some(home) = home_weak.upgrade() {
                        let window = home.window();
                        if window.is_visible() {
                            let _ = window.hide();
                        } else {
                            let _ = window.show();
                        }
                    }
                }
            }

            while let Ok(event) = MenuEvent::receiver().try_recv() {
                let Some(home) = home_weak.upgrade() else { continue };
                if event.id == show_id {
                    let _ = home.window().show();
                } else if event.id == hide_id {
                    let _ = home.window().hide();
                } else if event.id == quit_id {
                    let _ = slint::quit_event_loop();
                }
            }
        });

        TrayGuard {
            _tray_icon: tray_icon,
            _poll_timer: poll_timer,
        }
    }
}
