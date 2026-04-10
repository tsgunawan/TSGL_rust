use std::env;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::{App, CreationContext, NativeOptions, egui};
use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};
use parking_lot::{Condvar, Mutex, RwLock};

const WINDOW_SIZE: [f32; 2] = [980.0, 820.0];
const DATA_COLS: usize = 10;
const DATA_ROWS: usize = 20;
const CELL_SIZE: f32 = 20.0;
const CELL_GAP: f32 = 2.0;
const DATA_LEFT: f32 = 390.0;
const DATA_TOP: f32 = 160.0;
const WRITER_HOME_X: f32 = 120.0;
const WRITER_WAIT_X: f32 = 270.0;
const WRITER_ACCESS_X: f32 = 345.0;
const READER_HOME_X: f32 = 860.0;
const READER_WAIT_X: f32 = 710.0;
const READER_ACCESS_X: f32 = 640.0;
const THREAD_START_Y: f32 = 220.0;
const THREAD_Y_STEP: f32 = 54.0;
const MOVE_DURATION_MS: u64 = 220;
const START_STAGGER_MS: u64 = 90;

fn main() -> eframe::Result<()> {
    let config = ReaderWriterConfig::from_args();
    let title = format!(
        "Reader-Writer ({}, {} readers, {} writers)",
        config.policy.label(),
        config.reader_count,
        config.writer_count
    );

    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([860.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| Box::new(ReaderWriterApp::new(cc, config.clone()))),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockPolicy {
    ReaderPriority,
    WriterPriority,
    Fair,
}

impl LockPolicy {
    fn from_flag(flag: &str) -> Self {
        match flag {
            "r" | "reader" => Self::ReaderPriority,
            "w" | "writer" => Self::WriterPriority,
            _ => Self::Fair,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ReaderPriority => "Reader priority",
            Self::WriterPriority => "Writer priority",
            Self::Fair => "Fair priority",
        }
    }
}

#[derive(Clone, Debug)]
struct ReaderWriterConfig {
    reader_count: usize,
    writer_count: usize,
    policy: LockPolicy,
    starved: bool,
}

impl ReaderWriterConfig {
    fn from_args() -> Self {
        let mut args = env::args().skip(1);
        let reader_count = args
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .map(|n| n.clamp(1, 9))
            .unwrap_or(6);
        let writer_count = args
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .map(|n| n.clamp(1, 9))
            .unwrap_or(6);
        let policy = args
            .next()
            .map(|s| LockPolicy::from_flag(&s.to_lowercase()))
            .unwrap_or(LockPolicy::Fair);
        let starved = args
            .next()
            .map(|s| s.eq_ignore_ascii_case("s"))
            .unwrap_or(false);

        Self {
            reader_count,
            writer_count,
            policy,
            starved,
        }
    }

    fn timing(&self) -> TimingProfile {
        let total_threads = self.reader_count + self.writer_count;
        let base_access = if total_threads == 0 {
            1.0
        } else {
            1.0 / total_threads as f32
        };

        if self.starved {
            match self.policy {
                LockPolicy::ReaderPriority => TimingProfile {
                    wait_min_tenths: 0,
                    wait_range_tenths: 20,
                    access_seconds: base_access * 20.0,
                },
                LockPolicy::WriterPriority | LockPolicy::Fair => TimingProfile {
                    wait_min_tenths: 2,
                    wait_range_tenths: 10,
                    access_seconds: base_access,
                },
            }
        } else {
            TimingProfile {
                wait_min_tenths: 20,
                wait_range_tenths: 50,
                access_seconds: base_access,
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TimingProfile {
    wait_min_tenths: u64,
    wait_range_tenths: u64,
    access_seconds: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadKind {
    Reader,
    Writer,
}

impl ThreadKind {
    fn label(self) -> &'static str {
        match self {
            Self::Reader => "Reader",
            Self::Writer => "Writer",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadStatus {
    Thinking,
    Waiting,
    Reading,
    Writing,
    Stopped,
}

impl ThreadStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Thinking => "Thinking",
            Self::Waiting => "Waiting",
            Self::Reading => "Reading",
            Self::Writing => "Writing",
            Self::Stopped => "Stopped",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Motion {
    from: Pos2,
    to: Pos2,
    started_at: Instant,
    duration: Duration,
}

impl Motion {
    fn stationary(at: Pos2) -> Self {
        Self {
            from: at,
            to: at,
            started_at: Instant::now(),
            duration: Duration::from_millis(1),
        }
    }

    fn value_at(&self, now: Instant) -> Pos2 {
        let elapsed = now.saturating_duration_since(self.started_at);
        let denom = self.duration.as_secs_f32().max(0.001);
        let t = (elapsed.as_secs_f32() / denom).clamp(0.0, 1.0);
        Pos2::new(
            self.from.x + (self.to.x - self.from.x) * t,
            self.from.y + (self.to.y - self.from.y) * t,
        )
    }
}

#[derive(Clone, Debug)]
struct ThreadVisual {
    kind: ThreadKind,
    id: usize,
    count: usize,
    color: Color32,
    status: ThreadStatus,
    target_cell: Option<usize>,
    motion: Motion,
}

impl ThreadVisual {
    fn current_position(&self, now: Instant) -> Pos2 {
        self.motion.value_at(now)
    }
}

#[derive(Clone, Debug)]
struct DataCell {
    color: Color32,
    read_count: usize,
    write_count: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct CoordinatorSnapshot {
    active_readers: usize,
    active_writers: usize,
    waiting_readers: usize,
    waiting_writers: usize,
    data_count: usize,
}

#[derive(Debug)]
struct AppState {
    policy: LockPolicy,
    max_cells: usize,
    cells: Vec<DataCell>,
    threads: Vec<ThreadVisual>,
    coordinator: CoordinatorSnapshot,
}

impl AppState {
    fn new(config: &ReaderWriterConfig) -> Self {
        let mut threads = Vec::with_capacity(config.reader_count + config.writer_count);
        for id in 0..config.writer_count {
            let home = writer_home_pos(id);
            threads.push(ThreadVisual {
                kind: ThreadKind::Writer,
                id,
                count: 0,
                color: Colors::writer_palette(id),
                status: ThreadStatus::Thinking,
                target_cell: None,
                motion: Motion::stationary(home),
            });
        }
        for id in 0..config.reader_count {
            let home = reader_home_pos(id);
            threads.push(ThreadVisual {
                kind: ThreadKind::Reader,
                id,
                count: 0,
                color: Colors::reader_palette(id),
                status: ThreadStatus::Thinking,
                target_cell: None,
                motion: Motion::stationary(home),
            });
        }

        Self {
            policy: config.policy,
            max_cells: DATA_COLS * DATA_ROWS,
            cells: Vec::new(),
            threads,
            coordinator: CoordinatorSnapshot::default(),
        }
    }

    fn thread_index(&self, kind: ThreadKind, id: usize) -> usize {
        match kind {
            ThreadKind::Writer => id,
            ThreadKind::Reader => self
                .threads
                .iter()
                .position(|thread| thread.kind == ThreadKind::Reader && thread.id == id)
                .expect("reader thread must exist"),
        }
    }

    fn set_thread_motion(
        &mut self,
        kind: ThreadKind,
        id: usize,
        status: ThreadStatus,
        target_cell: Option<usize>,
        destination: Pos2,
        duration: Duration,
    ) {
        let index = self.thread_index(kind, id);
        let now = Instant::now();
        let current = self.threads[index].current_position(now);
        self.threads[index].status = status;
        self.threads[index].target_cell = target_cell;
        self.threads[index].motion = Motion {
            from: current,
            to: destination,
            started_at: now,
            duration,
        };
    }

    fn finish_access(
        &mut self,
        kind: ThreadKind,
        id: usize,
        new_color: Color32,
        home: Pos2,
        status: ThreadStatus,
    ) {
        let index = self.thread_index(kind, id);
        let now = Instant::now();
        let current = self.threads[index].current_position(now);
        self.threads[index].status = status;
        self.threads[index].target_cell = None;
        self.threads[index].count += 1;
        self.threads[index].color = new_color;
        self.threads[index].motion = Motion {
            from: current,
            to: home,
            started_at: now,
            duration: Duration::from_millis(MOVE_DURATION_MS),
        };
    }
}

struct ReaderWriterApp {
    config: ReaderWriterConfig,
    shared: Arc<RwLock<AppState>>,
    runtime: Option<WorkerRuntime>,
}

impl ReaderWriterApp {
    fn new(_cc: &CreationContext<'_>, config: ReaderWriterConfig) -> Self {
        let (shared, runtime) = Self::build_runtime(&config);
        Self {
            config,
            shared,
            runtime: Some(runtime),
        }
    }

    fn build_runtime(config: &ReaderWriterConfig) -> (Arc<RwLock<AppState>>, WorkerRuntime) {
        let shared = Arc::new(RwLock::new(AppState::new(config)));
        let coordinator = Arc::new(LockCoordinator::new(config.policy));
        {
            let snapshot = coordinator.snapshot();
            shared.write().coordinator = snapshot;
        }
        let runtime = WorkerRuntime::spawn(config.clone(), Arc::clone(&shared), coordinator);
        (shared, runtime)
    }

    fn reset_runtime(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.stop();
        }
        let (shared, runtime) = Self::build_runtime(&self.config);
        self.shared = shared;
        self.runtime = Some(runtime);
    }

    fn draw_scene(&self, ui: &mut egui::Ui) {
        let now = Instant::now();
        let painter = ui.painter();
        let app = self.shared.read();

        painter.rect_filled(ui.max_rect(), 0.0, Color32::from_rgb(248, 248, 245));

        let grid_rect = data_grid_rect();
        let margin_rect = grid_rect.expand2(Vec2::new(52.0, 0.0));
        painter.rect_filled(margin_rect, 8.0, Color32::from_rgb(232, 232, 228));
        painter.rect_filled(grid_rect, 8.0, Color32::from_rgb(214, 214, 214));
        painter.rect_stroke(grid_rect, 8.0, Stroke::new(1.5, Color32::BLACK));

        painter.text(
            Pos2::new(WINDOW_SIZE[0] * 0.5, 92.0),
            Align2::CENTER_CENTER,
            app.policy.label(),
            FontId::proportional(30.0),
            Color32::BLACK,
        );
        painter.text(
            Pos2::new(WINDOW_SIZE[0] * 0.5, 122.0),
            Align2::CENTER_CENTER,
            format!("Shared Data Store  {}/{}", app.cells.len(), app.max_cells),
            FontId::proportional(20.0),
            Color32::from_rgb(55, 55, 55),
        );

        painter.line_segment(
            [Pos2::new(325.0, DATA_TOP), Pos2::new(325.0, DATA_TOP + grid_rect.height())],
            Stroke::new(1.0, Color32::BLACK),
        );
        painter.line_segment(
            [Pos2::new(655.0, DATA_TOP), Pos2::new(655.0, DATA_TOP + grid_rect.height())],
            Stroke::new(1.0, Color32::BLACK),
        );

        painter.text(
            Pos2::new(120.0, 130.0),
            Align2::CENTER_CENTER,
            "Writers",
            FontId::proportional(26.0),
            Color32::BLACK,
        );
        painter.text(
            Pos2::new(860.0, 130.0),
            Align2::CENTER_CENTER,
            "Readers",
            FontId::proportional(26.0),
            Color32::BLACK,
        );
        painter.text(
            Pos2::new(235.0, 125.0),
            Align2::CENTER_CENTER,
            "Waiting",
            FontId::proportional(18.0),
            Color32::from_gray(120),
        );
        painter.text(
            Pos2::new(760.0, 125.0),
            Align2::CENTER_CENTER,
            "Waiting",
            FontId::proportional(18.0),
            Color32::from_gray(120),
        );

        for idx in 0..app.max_cells {
            let rect = cell_rect(idx);
            let cell = app.cells.get(idx);
            let fill = cell.map(|c| c.color).unwrap_or(Color32::from_rgb(240, 240, 240));
            painter.rect_filled(rect, 2.0, fill);
            painter.rect_stroke(rect, 2.0, Stroke::new(1.0, Color32::from_gray(110)));
        }

        for thread in &app.threads {
            let pos = thread.current_position(now);
            if let Some(target) = thread.target_cell {
                let target_center = cell_center(target);
                let arrow_color = match thread.kind {
                    ThreadKind::Reader => Color32::from_rgb(50, 50, 50),
                    ThreadKind::Writer => Color32::from_rgb(30, 30, 30),
                };
                painter.line_segment([pos, target_center], Stroke::new(1.5, arrow_color));
            }
        }

        for thread in &app.threads {
            let pos = thread.current_position(now);
            let state_color = match thread.status {
                ThreadStatus::Thinking => thread.color.gamma_multiply(0.88),
                ThreadStatus::Waiting => Color32::from_rgb(30, 30, 30),
                ThreadStatus::Reading | ThreadStatus::Writing => thread.color,
                ThreadStatus::Stopped => Color32::from_rgb(80, 80, 80),
            };

            match thread.kind {
                ThreadKind::Writer => {
                    painter.circle_filled(pos, 14.0, state_color);
                    painter.circle_stroke(pos, 14.0, Stroke::new(2.0, Color32::BLACK));
                }
                ThreadKind::Reader => {
                    painter.circle_filled(pos, 14.0, state_color);
                    painter.circle_stroke(pos, 14.0, Stroke::new(2.0, Color32::BLACK));
                }
            }

            painter.text(
                pos + Vec2::new(0.0, 0.5),
                Align2::CENTER_CENTER,
                thread.count.to_string(),
                FontId::proportional(15.0),
                Color32::WHITE,
            );

            let label_pos = match thread.kind {
                ThreadKind::Writer => pos + Vec2::new(-70.0, 0.0),
                ThreadKind::Reader => pos + Vec2::new(70.0, 0.0),
            };
            painter.text(
                label_pos,
                Align2::CENTER_CENTER,
                format!("{} {}", thread.kind.label(), thread.id + 1),
                FontId::proportional(15.0),
                Color32::BLACK,
            );

            let state_pos = pos + Vec2::new(0.0, 24.0);
            painter.text(
                state_pos,
                Align2::CENTER_CENTER,
                thread.status.label(),
                FontId::proportional(13.0),
                Color32::from_rgb(65, 65, 65),
            );
        }

        let footer = format!(
            "Cells: {}   Active readers: {}   Active writers: {}   Waiting readers: {}   Waiting writers: {}",
            app.coordinator.data_count,
            app.coordinator.active_readers,
            app.coordinator.active_writers,
            app.coordinator.waiting_readers,
            app.coordinator.waiting_writers
        );
        painter.text(
            Pos2::new(WINDOW_SIZE[0] * 0.5, WINDOW_SIZE[1] - 38.0),
            Align2::CENTER_CENTER,
            footer,
            FontId::proportional(18.0),
            Color32::from_rgb(40, 40, 40),
        );

        let legend_top = WINDOW_SIZE[1] - 110.0;
        painter.text(
            Pos2::new(WINDOW_SIZE[0] * 0.5, legend_top - 26.0),
            Align2::CENTER_CENTER,
            "Legend",
            FontId::proportional(18.0),
            Color32::from_rgb(45, 45, 45),
        );
        draw_legend(
            painter,
            Pos2::new(WINDOW_SIZE[0] * 0.5 - 190.0, legend_top),
            "Waiting",
            Color32::from_rgb(30, 30, 30),
        );
        draw_legend(
            painter,
            Pos2::new(WINDOW_SIZE[0] * 0.5, legend_top),
            "Reading / Writing",
            Color32::from_rgb(60, 150, 90),
        );
        draw_legend(
            painter,
            Pos2::new(WINDOW_SIZE[0] * 0.5 + 200.0, legend_top),
            "Thinking (thread color)",
            Colors::reader_palette(0),
        );
    }
}

impl App for ReaderWriterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));
        let mut should_reset = false;
        let space_pressed = ctx.input(|input| input.key_pressed(egui::Key::Space));

        if space_pressed && let Some(runtime) = self.runtime.as_ref() {
            let paused = runtime.is_paused();
            runtime.set_paused(!paused);
        }

        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let paused = self
                    .runtime
                    .as_ref()
                    .map(|runtime| runtime.is_paused())
                    .unwrap_or(false);
                let pause_label = if paused { "Resume" } else { "Pause" };
                if ui.button(pause_label).clicked()
                    && let Some(runtime) = self.runtime.as_ref()
                {
                    runtime.set_paused(!paused);
                }
                if ui.button("Reset").clicked() {
                    should_reset = true;
                }
                ui.separator();
                ui.label(format!(
                    "{} readers, {} writers, {}",
                    self.config.reader_count,
                    self.config.writer_count,
                    self.config.policy.label()
                ));
                ui.separator();
                ui.label("Space: pause/resume");
            });
        });

        if should_reset {
            self.reset_runtime();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_scene(ui);
        });
    }
}

impl Drop for ReaderWriterApp {
    fn drop(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.stop();
        }
    }
}

struct WorkerRuntime {
    running: Arc<AtomicBool>,
    control: Arc<RunControl>,
    coordinator: Arc<LockCoordinator>,
    handles: Vec<JoinHandle<()>>,
}

impl WorkerRuntime {
    fn spawn(
        config: ReaderWriterConfig,
        shared: Arc<RwLock<AppState>>,
        coordinator: Arc<LockCoordinator>,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let control = Arc::new(RunControl::new());
        let timing = config.timing();
        let mut handles = Vec::new();

        for id in 0..config.writer_count {
            let running = Arc::clone(&running);
            let control = Arc::clone(&control);
            let shared = Arc::clone(&shared);
            let coordinator = Arc::clone(&coordinator);
            let timing = timing;
            handles.push(thread::spawn(move || {
                thread::sleep(Duration::from_millis(id as u64 * START_STAGGER_MS));
                writer_worker(id, shared, coordinator, running, control, timing);
            }));
        }

        for id in 0..config.reader_count {
            let running = Arc::clone(&running);
            let control = Arc::clone(&control);
            let shared = Arc::clone(&shared);
            let coordinator = Arc::clone(&coordinator);
            let writer_count = config.writer_count;
            let timing = timing;
            handles.push(thread::spawn(move || {
                thread::sleep(Duration::from_millis(
                    writer_count as u64 * START_STAGGER_MS + (id as u64 + 1) * START_STAGGER_MS,
                ));
                reader_worker(id, shared, coordinator, running, control, timing);
            }));
        }

        Self {
            running,
            control,
            coordinator,
            handles,
        }
    }

    fn is_paused(&self) -> bool {
        self.control.is_paused()
    }

    fn set_paused(&self, paused: bool) {
        self.control.set_paused(paused);
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.control.wake_all();
        self.coordinator.wake_all();
        while let Some(handle) = self.handles.pop() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
struct RunControl {
    state: Mutex<ControlState>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct ControlState {
    paused: bool,
}

impl RunControl {
    fn new() -> Self {
        Self {
            state: Mutex::new(ControlState::default()),
            cv: Condvar::new(),
        }
    }

    fn is_paused(&self) -> bool {
        self.state.lock().paused
    }

    fn set_paused(&self, paused: bool) {
        self.state.lock().paused = paused;
        if !paused {
            self.cv.notify_all();
        }
    }

    fn wait_if_paused(&self, running: &AtomicBool) -> bool {
        let mut guard = self.state.lock();
        while guard.paused && running.load(Ordering::Relaxed) {
            self.cv.wait_for(&mut guard, Duration::from_millis(100));
        }
        running.load(Ordering::Relaxed)
    }

    fn wake_all(&self) {
        self.cv.notify_all();
    }
}

#[derive(Debug)]
struct LockCoordinator {
    policy: LockPolicy,
    state: Mutex<CoordinatorState>,
    readers_cv: Condvar,
    writers_cv: Condvar,
}

#[derive(Debug)]
struct CoordinatorState {
    active_readers: usize,
    active_writers: usize,
    waiting_readers: usize,
    waiting_writers: usize,
    data_count: usize,
    writer_turn: bool,
}

impl LockCoordinator {
    fn new(policy: LockPolicy) -> Self {
        Self {
            policy,
            state: Mutex::new(CoordinatorState {
                active_readers: 0,
                active_writers: 0,
                waiting_readers: 0,
                waiting_writers: 0,
                data_count: 0,
                writer_turn: false,
            }),
            readers_cv: Condvar::new(),
            writers_cv: Condvar::new(),
        }
    }

    fn read_lock(&self, running: &AtomicBool) -> bool {
        let mut guard = self.state.lock();
        guard.waiting_readers += 1;
        while running.load(Ordering::Relaxed) && self.reader_blocked(&guard) {
            self.readers_cv.wait_for(&mut guard, Duration::from_millis(100));
        }
        guard.waiting_readers = guard.waiting_readers.saturating_sub(1);
        if !running.load(Ordering::Relaxed) {
            return false;
        }
        guard.active_readers += 1;
        if self.policy == LockPolicy::Fair {
            guard.writer_turn = false;
        }
        true
    }

    fn read_unlock(&self) {
        let mut guard = self.state.lock();
        guard.active_readers = guard.active_readers.saturating_sub(1);
        if guard.active_readers == 0 {
            if self.policy == LockPolicy::Fair && guard.waiting_writers > 0 {
                guard.writer_turn = true;
            }
            self.writers_cv.notify_one();
        }
        self.readers_cv.notify_all();
    }

    fn write_lock(&self, running: &AtomicBool) -> bool {
        let mut guard = self.state.lock();
        guard.waiting_writers += 1;
        while running.load(Ordering::Relaxed) && self.writer_blocked(&guard) {
            self.writers_cv.wait_for(&mut guard, Duration::from_millis(100));
        }
        guard.waiting_writers = guard.waiting_writers.saturating_sub(1);
        if !running.load(Ordering::Relaxed) {
            return false;
        }
        guard.active_writers = 1;
        if self.policy == LockPolicy::Fair {
            guard.writer_turn = true;
        }
        true
    }

    fn write_unlock(&self, data_count: usize) {
        let mut guard = self.state.lock();
        guard.active_writers = 0;
        guard.data_count = data_count;
        if self.policy == LockPolicy::Fair && guard.waiting_readers > 0 {
            guard.writer_turn = false;
        }

        match self.policy {
            LockPolicy::ReaderPriority => {
                if guard.waiting_readers > 0 {
                    self.readers_cv.notify_all();
                } else {
                    self.writers_cv.notify_one();
                }
            }
            LockPolicy::WriterPriority => {
                if guard.waiting_writers > 0 {
                    self.writers_cv.notify_one();
                } else {
                    self.readers_cv.notify_all();
                }
            }
            LockPolicy::Fair => {
                if guard.waiting_readers > 0 {
                    self.readers_cv.notify_all();
                }
                if guard.waiting_writers > 0 {
                    self.writers_cv.notify_one();
                }
            }
        }
    }

    fn snapshot(&self) -> CoordinatorSnapshot {
        let guard = self.state.lock();
        CoordinatorSnapshot {
            active_readers: guard.active_readers,
            active_writers: guard.active_writers,
            waiting_readers: guard.waiting_readers,
            waiting_writers: guard.waiting_writers,
            data_count: guard.data_count,
        }
    }

    fn wake_all(&self) {
        self.readers_cv.notify_all();
        self.writers_cv.notify_all();
    }

    fn reader_blocked(&self, state: &CoordinatorState) -> bool {
        if state.data_count == 0 {
            return true;
        }
        if state.active_writers > 0 {
            return true;
        }
        match self.policy {
            LockPolicy::ReaderPriority => false,
            LockPolicy::WriterPriority => state.waiting_writers > 0,
            LockPolicy::Fair => state.writer_turn && state.waiting_writers > 0,
        }
    }

    fn writer_blocked(&self, state: &CoordinatorState) -> bool {
        // Match the C++ demo's practical startup behavior: writers must be able to seed
        // the shared store before reader-priority/fair rules begin to matter.
        if state.data_count == 0 {
            return state.active_writers > 0 || state.active_readers > 0;
        }
        if state.active_writers > 0 || state.active_readers > 0 {
            return true;
        }
        match self.policy {
            LockPolicy::ReaderPriority => state.waiting_readers > 0,
            LockPolicy::WriterPriority => false,
            LockPolicy::Fair => !state.writer_turn && state.waiting_readers > 0,
        }
    }
}

fn writer_worker(
    id: usize,
    shared: Arc<RwLock<AppState>>,
    coordinator: Arc<LockCoordinator>,
    running: Arc<AtomicBool>,
    control: Arc<RunControl>,
    timing: TimingProfile,
) {
    let mut rng = SmallRng::seeded(0xC0FFEE00 + id as u64 * 101);
    while running.load(Ordering::Relaxed) {
        if !control.wait_if_paused(&running) {
            break;
        }
        set_waiting_visual(&shared, ThreadKind::Writer, id, ThreadStatus::Thinking, None, writer_home_pos(id));
        if !sleep_with_stop(&running, &control, random_wait_duration(&mut rng, timing)) {
            break;
        }

        if !control.wait_if_paused(&running) {
            break;
        }
        set_waiting_visual(&shared, ThreadKind::Writer, id, ThreadStatus::Waiting, None, writer_wait_pos(id));
        if !coordinator.write_lock(&running) {
            break;
        }
        publish_snapshot(&shared, &coordinator);

        let (target_idx, color, new_data_count) = {
            let mut app = shared.write();
            let target_idx = if app.cells.len() < app.max_cells {
                app.cells.len()
            } else {
                rng.next_usize(app.cells.len())
            };
            let color = Colors::writer_palette(id).gamma_multiply(0.7 + rng.next_f32() * 0.3);
            if target_idx < app.cells.len() {
                let cell = &mut app.cells[target_idx];
                cell.color = color;
                cell.write_count += 1;
            } else if app.cells.len() < app.max_cells {
                app.cells.push(DataCell {
                    color,
                    read_count: 0,
                    write_count: 1,
                });
            }
            let data_count = app.cells.len();
            app.set_thread_motion(
                ThreadKind::Writer,
                id,
                ThreadStatus::Writing,
                Some(target_idx),
                writer_access_pos(id),
                Duration::from_millis(MOVE_DURATION_MS),
            );
            (target_idx, color, data_count)
        };

        publish_snapshot(&shared, &coordinator);
        if !sleep_with_stop(&running, &control, access_duration(timing)) {
            coordinator.write_unlock(new_data_count);
            publish_snapshot(&shared, &coordinator);
            break;
        }

        {
            let mut app = shared.write();
            let _ = target_idx;
            app.finish_access(
                ThreadKind::Writer,
                id,
                color,
                writer_home_pos(id),
                ThreadStatus::Thinking,
            );
        }

        coordinator.write_unlock(new_data_count);
        publish_snapshot(&shared, &coordinator);
    }

    stop_thread_visual(&shared, ThreadKind::Writer, id, writer_home_pos(id));
}

fn reader_worker(
    id: usize,
    shared: Arc<RwLock<AppState>>,
    coordinator: Arc<LockCoordinator>,
    running: Arc<AtomicBool>,
    control: Arc<RunControl>,
    timing: TimingProfile,
) {
    let mut rng = SmallRng::seeded(0xBAD5EED0 + id as u64 * 131);
    while running.load(Ordering::Relaxed) {
        if !control.wait_if_paused(&running) {
            break;
        }
        set_waiting_visual(&shared, ThreadKind::Reader, id, ThreadStatus::Thinking, None, reader_home_pos(id));
        if !sleep_with_stop(&running, &control, random_wait_duration(&mut rng, timing)) {
            break;
        }

        if !control.wait_if_paused(&running) {
            break;
        }
        set_waiting_visual(&shared, ThreadKind::Reader, id, ThreadStatus::Waiting, None, reader_wait_pos(id));
        if !coordinator.read_lock(&running) {
            break;
        }
        publish_snapshot(&shared, &coordinator);

        let (target_idx, color) = {
            let mut app = shared.write();
            if app.cells.is_empty() {
                coordinator.read_unlock();
                publish_snapshot(&shared, &coordinator);
                continue;
            }
            let target_idx = rng.next_usize(app.cells.len());
            let color = {
                let cell = &mut app.cells[target_idx];
                cell.read_count += 1;
                cell.color
            };
            app.set_thread_motion(
                ThreadKind::Reader,
                id,
                ThreadStatus::Reading,
                Some(target_idx),
                reader_access_pos(id),
                Duration::from_millis(MOVE_DURATION_MS),
            );
            (target_idx, color)
        };

        publish_snapshot(&shared, &coordinator);
        if !sleep_with_stop(&running, &control, access_duration(timing)) {
            coordinator.read_unlock();
            publish_snapshot(&shared, &coordinator);
            break;
        }

        {
            let mut app = shared.write();
            let _ = target_idx;
            app.finish_access(
                ThreadKind::Reader,
                id,
                color,
                reader_home_pos(id),
                ThreadStatus::Thinking,
            );
        }

        coordinator.read_unlock();
        publish_snapshot(&shared, &coordinator);
    }

    stop_thread_visual(&shared, ThreadKind::Reader, id, reader_home_pos(id));
}

fn publish_snapshot(shared: &Arc<RwLock<AppState>>, coordinator: &LockCoordinator) {
    let snapshot = coordinator.snapshot();
    shared.write().coordinator = snapshot;
}

fn set_waiting_visual(
    shared: &Arc<RwLock<AppState>>,
    kind: ThreadKind,
    id: usize,
    status: ThreadStatus,
    target: Option<usize>,
    pos: Pos2,
) {
    shared.write().set_thread_motion(
        kind,
        id,
        status,
        target,
        pos,
        Duration::from_millis(MOVE_DURATION_MS),
    );
}

fn stop_thread_visual(shared: &Arc<RwLock<AppState>>, kind: ThreadKind, id: usize, home: Pos2) {
    let mut app = shared.write();
    app.set_thread_motion(
        kind,
        id,
        ThreadStatus::Stopped,
        None,
        home,
        Duration::from_millis(120),
    );
}

fn sleep_with_stop(running: &AtomicBool, control: &RunControl, duration: Duration) -> bool {
    let chunk = Duration::from_millis(25);
    let start = Instant::now();
    while running.load(Ordering::Relaxed) && start.elapsed() < duration {
        if !control.wait_if_paused(running) {
            return false;
        }
        let remaining = duration.saturating_sub(start.elapsed());
        thread::sleep(remaining.min(chunk));
    }
    running.load(Ordering::Relaxed)
}

fn random_wait_duration(rng: &mut SmallRng, timing: TimingProfile) -> Duration {
    let tenths = timing.wait_min_tenths + rng.next_u64() % timing.wait_range_tenths.max(1);
    Duration::from_millis(tenths * 100)
}

fn access_duration(timing: TimingProfile) -> Duration {
    Duration::from_secs_f32(timing.access_seconds.max(0.01))
}

fn data_grid_rect() -> Rect {
    let width = DATA_COLS as f32 * (CELL_SIZE + CELL_GAP) - CELL_GAP;
    let height = DATA_ROWS as f32 * (CELL_SIZE + CELL_GAP) - CELL_GAP;
    Rect::from_min_size(Pos2::new(DATA_LEFT, DATA_TOP), Vec2::new(width, height))
}

fn cell_rect(index: usize) -> Rect {
    let col = index % DATA_COLS;
    let row = index / DATA_COLS;
    let min = Pos2::new(
        DATA_LEFT + col as f32 * (CELL_SIZE + CELL_GAP),
        DATA_TOP + row as f32 * (CELL_SIZE + CELL_GAP),
    );
    Rect::from_min_size(min, Vec2::splat(CELL_SIZE))
}

fn cell_center(index: usize) -> Pos2 {
    cell_rect(index).center()
}

fn writer_home_pos(id: usize) -> Pos2 {
    Pos2::new(WRITER_HOME_X, THREAD_START_Y + id as f32 * THREAD_Y_STEP)
}

fn writer_wait_pos(id: usize) -> Pos2 {
    Pos2::new(WRITER_WAIT_X, THREAD_START_Y + id as f32 * THREAD_Y_STEP)
}

fn writer_access_pos(id: usize) -> Pos2 {
    Pos2::new(WRITER_ACCESS_X, THREAD_START_Y + id as f32 * THREAD_Y_STEP)
}

fn reader_home_pos(id: usize) -> Pos2 {
    Pos2::new(READER_HOME_X, THREAD_START_Y + id as f32 * THREAD_Y_STEP)
}

fn reader_wait_pos(id: usize) -> Pos2 {
    Pos2::new(READER_WAIT_X, THREAD_START_Y + id as f32 * THREAD_Y_STEP)
}

fn reader_access_pos(id: usize) -> Pos2 {
    Pos2::new(READER_ACCESS_X, THREAD_START_Y + id as f32 * THREAD_Y_STEP)
}

fn draw_legend(painter: &egui::Painter, center: Pos2, label: &str, color: Color32) {
    painter.circle_filled(center, 12.0, color);
    painter.circle_stroke(center, 12.0, Stroke::new(1.5, Color32::BLACK));
    painter.text(
        center + Vec2::new(20.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(15.0),
        Color32::BLACK,
    );
}

struct Colors;

impl Colors {
    fn writer_palette(id: usize) -> Color32 {
        const PALETTE: [Color32; 9] = [
            Color32::from_rgb(199, 62, 58),
            Color32::from_rgb(214, 120, 48),
            Color32::from_rgb(212, 166, 40),
            Color32::from_rgb(72, 150, 92),
            Color32::from_rgb(63, 133, 191),
            Color32::from_rgb(120, 87, 194),
            Color32::from_rgb(186, 82, 146),
            Color32::from_rgb(86, 86, 86),
            Color32::from_rgb(38, 156, 156),
        ];
        PALETTE[id % PALETTE.len()]
    }

    fn reader_palette(id: usize) -> Color32 {
        const PALETTE: [Color32; 9] = [
            Color32::from_rgb(63, 133, 191),
            Color32::from_rgb(84, 171, 138),
            Color32::from_rgb(138, 120, 214),
            Color32::from_rgb(73, 156, 208),
            Color32::from_rgb(113, 174, 92),
            Color32::from_rgb(54, 169, 161),
            Color32::from_rgb(97, 121, 201),
            Color32::from_rgb(156, 133, 214),
            Color32::from_rgb(92, 160, 128),
        ];
        PALETTE[id % PALETTE.len()]
    }
}

struct SmallRng {
    state: u64,
}

impl SmallRng {
    fn seeded(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    fn next_f32(&mut self) -> f32 {
        (self.next_u64() as f64 / u64::MAX as f64) as f32
    }

    fn next_usize(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            0
        } else {
            (self.next_u64() as usize) % upper
        }
    }
}

