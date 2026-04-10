use std::env;
use std::f32::consts::{PI, TAU};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::{App, CreationContext, NativeOptions, egui};
use egui::{Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, Vec2};
use parking_lot::{Condvar, Mutex, RwLock};

const WINDOW_SIZE: [f32; 2] = [1120.0, 840.0];
const TABLE_CENTER_X: f32 = 420.0;
const TABLE_CENTER_Y: f32 = 425.0;
const TABLE_RADIUS: f32 = 250.0;
const FORK_HELD_RADIUS: f32 = 200.0;
const FORK_FREE_RADIUS: f32 = 170.0;
const PHIL_RADIUS: f32 = 32.0;
const LEGEND_X: f32 = 845.0;
const LEGEND_Y: f32 = 110.0;

fn main() -> eframe::Result<()> {
    let config = DiningConfig::from_args();
    let title = format!(
        "Dining Philosophers ({} philosophers, {}, {})",
        config.count,
        if config.step_mode {
            "step-through".to_owned()
        } else {
            format!("speed {}", config.speed)
        },
        config.method.label()
    );

    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([960.0, 760.0]),
        ..Default::default()
    };

    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| Box::new(DiningApp::new(cc, config.clone()))),
    )
}

#[derive(Clone, Debug)]
struct DiningConfig {
    count: usize,
    speed: u32,
    step_mode: bool,
    method: PhilMethod,
}

impl DiningConfig {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().skip(1).collect();
        let count = args
            .first()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n >= 2)
            .unwrap_or(5);

        let second = args.get(1).cloned().unwrap_or_else(|| "5".to_owned());
        let step_mode = second.eq_ignore_ascii_case("t") || second.eq_ignore_ascii_case("y");
        let speed = if step_mode {
            5
        } else {
            second
                .parse::<u32>()
                .ok()
                .filter(|v| *v > 0)
                .unwrap_or(5)
        };

        let method = args
            .get(2)
            .and_then(|s| s.chars().next())
            .map(PhilMethod::from_char)
            .unwrap_or(PhilMethod::OddEven);

        Self {
            count,
            speed,
            step_mode,
            method,
        }
    }

    fn frame_duration(&self) -> Duration {
        Duration::from_secs_f32((1.0 / self.speed as f32).max(0.02))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhilState {
    HasNone,
    HasRight,
    HasLeft,
    HasBoth,
    IsFull,
    Thinking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhilAction {
    DoNothing,
    TryLeft,
    TryRight,
    TryBoth,
    ReleaseLeft,
    ReleaseRight,
    ReleaseBoth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhilMethod {
    ForfeitWhenBlocked,
    WaitWhenBlocked,
    NCountRelease,
    ResourceHierarchy,
    OddEven,
}

impl PhilMethod {
    fn from_char(ch: char) -> Self {
        match ch {
            'w' | 'W' => Self::WaitWhenBlocked,
            'f' | 'F' => Self::ForfeitWhenBlocked,
            'n' | 'N' => Self::NCountRelease,
            'r' | 'R' => Self::ResourceHierarchy,
            'o' | 'O' => Self::OddEven,
            _ => Self::OddEven,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ForfeitWhenBlocked => "forfeit when blocked",
            Self::WaitWhenBlocked => "wait when blocked",
            Self::NCountRelease => "release on nth count",
            Self::ResourceHierarchy => "hierarchical resources",
            Self::OddEven => "odd-even check",
        }
    }
}

#[derive(Clone, Debug)]
struct Philosopher {
    id: usize,
    state: PhilState,
    action: PhilAction,
    meals: usize,
}

#[derive(Clone, Debug)]
struct ForkState {
    id: usize,
    user: Option<usize>,
}

#[derive(Debug)]
struct AppState {
    method: PhilMethod,
    count: usize,
    counter: usize,
    philosophers: Vec<Philosopher>,
    forks: Vec<ForkState>,
}

impl AppState {
    fn new(config: &DiningConfig) -> Self {
        let philosophers = (0..config.count)
            .map(|id| Philosopher {
                id,
                state: PhilState::Thinking,
                action: PhilAction::DoNothing,
                meals: 0,
            })
            .collect();
        let forks = (0..config.count)
            .map(|id| ForkState { id, user: None })
            .collect();

        Self {
            method: config.method,
            count: config.count,
            counter: 0,
            philosophers,
            forks,
        }
    }

    fn left_fork(&self, id: usize) -> usize {
        id
    }

    fn right_fork(&self, id: usize) -> usize {
        (id + self.count - 1) % self.count
    }

    fn fork_free(&self, fork_id: usize) -> bool {
        self.forks[fork_id].user.is_none()
    }

    fn check_step_for(&mut self, id: usize) {
        if self.philosophers[id].state == PhilState::IsFull {
            self.philosophers[id].state = PhilState::Thinking;
            self.philosophers[id].action = PhilAction::DoNothing;
            return;
        }

        match self.method {
            PhilMethod::ForfeitWhenBlocked => self.forfeit_when_blocked(id),
            PhilMethod::WaitWhenBlocked => self.wait_when_blocked(id),
            PhilMethod::NCountRelease => self.n_count_release(id),
            PhilMethod::ResourceHierarchy => self.resource_hierarchy(id),
            PhilMethod::OddEven => self.odd_even(id),
        }
        self.counter = self.counter.wrapping_add(1);
    }

    fn forfeit_when_blocked(&mut self, id: usize) {
        let left = self.left_fork(id);
        let right = self.right_fork(id);
        match self.philosophers[id].state {
            PhilState::HasNone => {
                self.philosophers[id].action = if self.fork_free(right) {
                    PhilAction::TryRight
                } else if self.fork_free(left) {
                    PhilAction::TryLeft
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasRight => {
                self.philosophers[id].action = if self.fork_free(left) {
                    PhilAction::TryLeft
                } else {
                    PhilAction::ReleaseRight
                };
            }
            PhilState::HasLeft => {
                self.philosophers[id].action = if self.fork_free(right) {
                    PhilAction::TryRight
                } else {
                    PhilAction::ReleaseLeft
                };
            }
            PhilState::HasBoth => self.philosophers[id].action = PhilAction::ReleaseBoth,
            PhilState::Thinking => self.philosophers[id].state = PhilState::HasNone,
            PhilState::IsFull => {}
        }
    }

    fn wait_when_blocked(&mut self, id: usize) {
        let left = self.left_fork(id);
        let right = self.right_fork(id);
        match self.philosophers[id].state {
            PhilState::HasNone => {
                self.philosophers[id].action = if self.fork_free(right) {
                    PhilAction::TryRight
                } else if self.fork_free(left) {
                    PhilAction::TryLeft
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasRight => {
                self.philosophers[id].action = if self.fork_free(left) {
                    PhilAction::TryLeft
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasLeft => {
                self.philosophers[id].action = if self.fork_free(right) {
                    PhilAction::TryRight
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasBoth => self.philosophers[id].action = PhilAction::ReleaseBoth,
            PhilState::Thinking => self.think(id),
            PhilState::IsFull => {}
        }
    }

    fn n_count_release(&mut self, id: usize) {
        let left = self.left_fork(id);
        let right = self.right_fork(id);
        match self.philosophers[id].state {
            PhilState::HasNone => {
                self.philosophers[id].action = if self.fork_free(right) {
                    PhilAction::TryRight
                } else if self.fork_free(left) {
                    PhilAction::TryLeft
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasRight => {
                self.philosophers[id].action = if self.fork_free(left) {
                    PhilAction::TryLeft
                } else if id == (self.counter % self.count + 1) {
                    PhilAction::ReleaseRight
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasLeft => {
                self.philosophers[id].action = if self.fork_free(right) {
                    PhilAction::TryRight
                } else if id == (self.counter % self.count + 1) {
                    PhilAction::ReleaseLeft
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasBoth => self.philosophers[id].action = PhilAction::ReleaseBoth,
            PhilState::Thinking => self.think(id),
            PhilState::IsFull => {}
        }
    }

    fn resource_hierarchy(&mut self, id: usize) {
        let left = self.left_fork(id);
        let right = self.right_fork(id);
        match self.philosophers[id].state {
            PhilState::HasNone => {
                self.philosophers[id].action = if right < left {
                    if self.fork_free(right) {
                        PhilAction::TryRight
                    } else {
                        PhilAction::DoNothing
                    }
                } else if self.fork_free(left) {
                    PhilAction::TryLeft
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasRight => {
                self.philosophers[id].action = if self.fork_free(left) {
                    PhilAction::TryLeft
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasLeft => {
                self.philosophers[id].action = if self.fork_free(right) {
                    PhilAction::TryRight
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasBoth => self.philosophers[id].action = PhilAction::ReleaseBoth,
            PhilState::Thinking => self.think(id),
            PhilState::IsFull => {}
        }
    }

    fn odd_even(&mut self, id: usize) {
        match self.philosophers[id].state {
            PhilState::HasNone => {
                self.philosophers[id].action = if (id % 2) == (self.counter % 2) {
                    PhilAction::TryBoth
                } else {
                    PhilAction::DoNothing
                };
            }
            PhilState::HasRight => {
                self.philosophers[id].action = if (id % 2) == (self.counter % 2) {
                    PhilAction::TryLeft
                } else {
                    PhilAction::ReleaseRight
                };
            }
            PhilState::HasLeft => {
                self.philosophers[id].action = if (id % 2) == (self.counter % 2) {
                    PhilAction::TryRight
                } else {
                    PhilAction::ReleaseLeft
                };
            }
            PhilState::HasBoth => self.philosophers[id].action = PhilAction::ReleaseBoth,
            PhilState::Thinking => self.think(id),
            PhilState::IsFull => {}
        }
    }

    fn think(&mut self, id: usize) {
        if ((self.counter + id) % 3) == 0 {
            self.philosophers[id].state = PhilState::HasNone;
            self.philosophers[id].action = PhilAction::DoNothing;
        }
    }

    fn act_step_for(&mut self, id: usize) {
        let left = self.left_fork(id);
        let right = self.right_fork(id);
        let action = self.philosophers[id].action;
        match action {
            PhilAction::TryLeft => {
                let _ = self.acquire(id, left);
            }
            PhilAction::TryRight => {
                let _ = self.acquire(id, right);
            }
            PhilAction::TryBoth => {
                let _ = self.acquire(id, left);
                let _ = self.acquire(id, right);
            }
            PhilAction::ReleaseLeft => {
                let _ = self.release(id, left);
            }
            PhilAction::ReleaseRight => {
                let _ = self.release(id, right);
            }
            PhilAction::ReleaseBoth => {
                let _ = self.release(id, left);
                let _ = self.release(id, right);
            }
            PhilAction::DoNothing => {}
        }
    }

    fn acquire(&mut self, phil_id: usize, fork_id: usize) -> bool {
        if self.forks[fork_id].user.is_some() {
            return false;
        }
        let right = self.right_fork(phil_id);
        let left = self.left_fork(phil_id);
        let state = &mut self.philosophers[phil_id].state;
        if fork_id == left {
            match *state {
                PhilState::HasNone => *state = PhilState::HasLeft,
                PhilState::HasRight => *state = PhilState::HasBoth,
                _ => return false,
            }
        } else if fork_id == right {
            match *state {
                PhilState::HasNone => *state = PhilState::HasRight,
                PhilState::HasLeft => *state = PhilState::HasBoth,
                _ => return false,
            }
        } else {
            return false;
        }
        self.forks[fork_id].user = Some(phil_id);
        true
    }

    fn release(&mut self, phil_id: usize, fork_id: usize) -> bool {
        if self.forks[fork_id].user != Some(phil_id) {
            return false;
        }
        let current = self.philosophers[phil_id].state;
        if current != PhilState::IsFull {
            let matching = if fork_id == self.left_fork(phil_id) {
                PhilState::HasLeft
            } else {
                PhilState::HasRight
            };
            self.philosophers[phil_id].state = if current == matching {
                PhilState::HasNone
            } else {
                self.philosophers[phil_id].meals += 1;
                PhilState::IsFull
            };
        }
        self.forks[fork_id].user = None;
        true
    }
}

struct DiningApp {
    config: DiningConfig,
    shared: Arc<RwLock<AppState>>,
    runtime: Option<Runtime>,
}

impl DiningApp {
    fn new(_cc: &CreationContext<'_>, config: DiningConfig) -> Self {
        let (shared, runtime) = Self::build_runtime(&config);
        Self {
            config,
            shared,
            runtime: Some(runtime),
        }
    }

    fn build_runtime(config: &DiningConfig) -> (Arc<RwLock<AppState>>, Runtime) {
        let shared = Arc::new(RwLock::new(AppState::new(config)));
        let runtime = Runtime::spawn(config.clone(), Arc::clone(&shared));
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
        let painter = ui.painter();
        let app = self.shared.read();

        painter.rect_filled(ui.max_rect(), 0.0, Color32::from_rgb(248, 248, 246));

        painter.circle_filled(
            Pos2::new(TABLE_CENTER_X, TABLE_CENTER_Y),
            TABLE_RADIUS - 48.0,
            Color32::from_rgb(80, 80, 80),
        );
        painter.circle_stroke(
            Pos2::new(TABLE_CENTER_X, TABLE_CENTER_Y),
            TABLE_RADIUS - 48.0,
            Stroke::new(2.0, Color32::BLACK),
        );

        for phil in &app.philosophers {
            let pos = philosopher_position(phil.id, app.count);
            painter.circle_filled(pos, PHIL_RADIUS, phil_color(phil.state));
            painter.circle_stroke(pos, PHIL_RADIUS, Stroke::new(2.0, Color32::BLACK));
            painter.text(
                pos,
                Align2::CENTER_CENTER,
                (phil.id + 1).to_string(),
                FontId::proportional(18.0),
                Color32::WHITE,
            );
            painter.text(
                pos + Vec2::new(0.0, 44.0),
                Align2::CENTER_CENTER,
                state_label(phil.state),
                FontId::proportional(14.0),
                Color32::from_rgb(55, 55, 55),
            );
            draw_meals(painter, phil.id, app.count, phil.meals);
        }

        for fork in &app.forks {
            let (center, angle, color) = fork_visual(&app, fork.id);
            draw_fork(painter, center, angle, color);
        }

        draw_legend(painter, &self.config, app.counter);
    }
}

impl App for DiningApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));
        let mut should_reset = false;
        let space_pressed = ctx.input(|input| input.key_pressed(egui::Key::Space));

        if space_pressed && let Some(runtime) = self.runtime.as_ref() {
            if self.config.step_mode {
                runtime.step_once();
            } else {
                let paused = runtime.is_paused();
                runtime.set_paused(!paused);
            }
        }

        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(runtime) = self.runtime.as_ref() {
                    if self.config.step_mode {
                        if ui.button("Step").clicked() {
                            runtime.step_once();
                        }
                    } else {
                        let paused = runtime.is_paused();
                        let label = if paused { "Resume" } else { "Pause" };
                        if ui.button(label).clicked() {
                            runtime.set_paused(!paused);
                        }
                    }
                }
                if ui.button("Reset").clicked() {
                    should_reset = true;
                }
                ui.separator();
                ui.label(format!(
                    "{} philosophers, {}, {}",
                    self.config.count,
                    if self.config.step_mode {
                        "step-through".to_owned()
                    } else {
                        format!("speed {}", self.config.speed)
                    },
                    self.config.method.label()
                ));
                ui.separator();
                ui.label(if self.config.step_mode {
                    "Space: step"
                } else {
                    "Space: pause/resume"
                });
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

impl Drop for DiningApp {
    fn drop(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.stop();
        }
    }
}

struct Runtime {
    running: Arc<AtomicBool>,
    control: Arc<RunControl>,
    handles: Vec<JoinHandle<()>>,
}

impl Runtime {
    fn spawn(config: DiningConfig, shared: Arc<RwLock<AppState>>) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let control = Arc::new(RunControl::new(config.step_mode));
        let barrier = if config.method == PhilMethod::ForfeitWhenBlocked {
            Some(Arc::new(Barrier::new(config.count)))
        } else {
            None
        };
        let mut handles = Vec::new();
        let frame_duration = config.frame_duration();

        for id in 0..config.count {
            let running = Arc::clone(&running);
            let control = Arc::clone(&control);
            let shared = Arc::clone(&shared);
            let barrier = barrier.clone();
            let method = config.method;
            handles.push(thread::spawn(move || {
                philosopher_worker(id, shared, running, control, barrier, method, frame_duration);
            }));
        }

        Self {
            running,
            control,
            handles,
        }
    }

    fn is_paused(&self) -> bool {
        self.control.is_paused()
    }

    fn set_paused(&self, paused: bool) {
        self.control.set_paused(paused);
    }

    fn step_once(&self) {
        self.control.step_once();
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.control.wake_all();
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

#[derive(Debug)]
struct ControlState {
    paused: bool,
    step_mode: bool,
    step_epoch: u64,
}

impl RunControl {
    fn new(step_mode: bool) -> Self {
        Self {
            state: Mutex::new(ControlState {
                paused: false,
                step_mode,
                step_epoch: 0,
            }),
            cv: Condvar::new(),
        }
    }

    fn is_paused(&self) -> bool {
        self.state.lock().paused
    }

    fn set_paused(&self, paused: bool) {
        let mut guard = self.state.lock();
        guard.paused = paused;
        if !paused {
            self.cv.notify_all();
        }
    }

    fn step_once(&self) {
        let mut guard = self.state.lock();
        guard.step_epoch = guard.step_epoch.wrapping_add(1);
        self.cv.notify_all();
    }

    fn wait_for_turn(&self, running: &AtomicBool, last_epoch: &mut u64) -> bool {
        let mut guard = self.state.lock();
        while running.load(Ordering::Relaxed) {
            if guard.step_mode {
                if guard.step_epoch > *last_epoch {
                    *last_epoch = guard.step_epoch;
                    return true;
                }
            } else if !guard.paused {
                return true;
            }
            self.cv.wait_for(&mut guard, Duration::from_millis(100));
        }
        false
    }

    fn wake_all(&self) {
        self.cv.notify_all();
    }
}

fn philosopher_worker(
    id: usize,
    shared: Arc<RwLock<AppState>>,
    running: Arc<AtomicBool>,
    control: Arc<RunControl>,
    barrier: Option<Arc<Barrier>>,
    method: PhilMethod,
    frame_duration: Duration,
) {
    let mut epoch = 0_u64;

    while running.load(Ordering::Relaxed) {
        if !control.wait_for_turn(&running, &mut epoch) {
            break;
        }
        {
            let mut app = shared.write();
            app.check_step_for(id);
        }

        if method == PhilMethod::ForfeitWhenBlocked {
            if let Some(barrier) = barrier.as_ref() {
                barrier.wait();
            }
        }

        {
            let mut app = shared.write();
            app.act_step_for(id);
        }

        if !sleep_with_control(&running, &control, frame_duration, &mut epoch) {
            break;
        }
    }
}

fn sleep_with_control(
    running: &AtomicBool,
    control: &RunControl,
    duration: Duration,
    epoch: &mut u64,
) -> bool {
    let start = Instant::now();
    let chunk = Duration::from_millis(20);
    while running.load(Ordering::Relaxed) && start.elapsed() < duration {
        if control.state.lock().step_mode {
            return running.load(Ordering::Relaxed);
        }
        if !control.wait_for_turn(running, epoch) {
            return false;
        }
        let remaining = duration.saturating_sub(start.elapsed());
        thread::sleep(remaining.min(chunk));
    }
    running.load(Ordering::Relaxed)
}

fn philosopher_position(id: usize, count: usize) -> Pos2 {
    let angle = id as f32 * TAU / count as f32;
    Pos2::new(
        TABLE_CENTER_X + TABLE_RADIUS * angle.cos(),
        TABLE_CENTER_Y + TABLE_RADIUS * angle.sin(),
    )
}

fn fork_visual(app: &AppState, fork_id: usize) -> (Pos2, f32, Color32) {
    let arc = TAU / app.count as f32;
    let close = 0.175_f32;
    let mut radius = FORK_HELD_RADIUS;
    let mut angle = (fork_id as f32 + 0.5) * arc;
    let mut color = Color32::BLACK;

    match app.forks[fork_id].user {
        Some(owner) if owner == fork_id => {
            angle = fork_id as f32 * arc + close;
            color = if app.philosophers[owner].state == PhilState::HasBoth {
                Color32::GREEN
            } else {
                Color32::from_rgb(150, 80, 200)
            };
        }
        Some(owner) if owner == (fork_id + 1) % app.count => {
            angle = ((fork_id + 1) as f32) * arc - close;
            color = if app.philosophers[owner].state == PhilState::HasBoth {
                Color32::GREEN
            } else {
                Color32::from_rgb(235, 155, 45)
            };
        }
        _ => {
            radius = FORK_FREE_RADIUS;
        }
    }

    let center = Pos2::new(
        TABLE_CENTER_X + radius * angle.cos(),
        TABLE_CENTER_Y + radius * angle.sin(),
    );
    (center, angle - PI / 2.0, color)
}

fn draw_fork(painter: &egui::Painter, center: Pos2, angle: f32, color: Color32) {
    let points = fork_points();
    let rotated: Vec<Pos2> = points
        .into_iter()
        .map(|point| rotate_and_translate(point, angle, center))
        .collect();
    painter.add(Shape::convex_polygon(
        rotated,
        color,
        Stroke::new(1.5, Color32::BLACK),
    ));
}

fn fork_points() -> Vec<Vec2> {
    let xscale = [
        0.0, 19.0, 19.0, 27.0, 27.0, 46.0, 46.0, 54.0, 54.0, 73.0, 73.0, 81.0, 81.0, 100.0,
        100.0, 65.0, 65.0, 35.0, 35.0, 0.0,
    ];
    let yscale = [
        0.0, 0.0, 20.0, 20.0, 0.0, 0.0, 20.0, 20.0, 0.0, 0.0, 20.0, 20.0, 0.0, 0.0, 30.0,
        30.0, 100.0, 100.0, 30.0, 30.0,
    ];
    let height = 42.0;
    let width = 12.0;
    xscale
        .iter()
        .zip(yscale.iter())
        .map(|(x, y)| Vec2::new(width * *x / 100.0, height * *y / 100.0) - Vec2::new(width / 2.0, height / 2.0))
        .collect()
}

fn rotate_and_translate(point: Vec2, angle: f32, center: Pos2) -> Pos2 {
    let rotated = Vec2::new(
        point.x * angle.cos() - point.y * angle.sin(),
        point.x * angle.sin() + point.y * angle.cos(),
    );
    center + rotated
}

fn draw_meals(painter: &egui::Painter, id: usize, count: usize, meals: usize) {
    if meals == 0 {
        return;
    }
    // Match the original TSGL arrangement:
    // angle = pangle + (meals/10) * 2π / RAD
    // dist  = BASEDIST + 8 * (meals % 10)
    // where integer division groups meals into radial stacks of 10.
    const BASE_DIST: f32 = TABLE_RADIUS + 64.0;
    let pangle = id as f32 * TAU / count as f32;
    let max_angle = pangle + TAU / count as f32;

    for meal_index in 0..meals {
        let column = meal_index / 10;
        let row = meal_index % 10;
        let angle = pangle + column as f32 * TAU / TABLE_RADIUS;
        if angle > max_angle {
            break;
        }
        let dist = BASE_DIST + 8.0 * row as f32;
        let pos = Pos2::new(
            TABLE_CENTER_X + dist * angle.cos(),
            TABLE_CENTER_Y + dist * angle.sin(),
        );
        painter.circle_filled(pos, 5.0, Color32::from_rgb(140, 90, 35));
        painter.circle_stroke(pos, 5.0, Stroke::new(1.0, Color32::from_rgb(90, 60, 20)));
    }

    let rendered_capacity = (((TAU / count as f32) * TABLE_RADIUS) / TAU).floor() as usize * 10;
    if meals > rendered_capacity.max(10) {
        let base = philosopher_position(id, count);
        painter.text(
            base + Vec2::new(0.0, -56.0),
            Align2::CENTER_CENTER,
            format!("x{}", meals),
            FontId::proportional(14.0),
            Color32::from_rgb(90, 60, 20),
        );
    }
}

fn draw_legend(painter: &egui::Painter, config: &DiningConfig, counter: usize) {
    let panel = Rect::from_min_size(Pos2::new(LEGEND_X, LEGEND_Y), Vec2::new(245.0, 560.0));
    painter.rect_filled(panel, 10.0, Color32::from_rgb(244, 244, 242));
    painter.rect_stroke(panel, 10.0, Stroke::new(1.5, Color32::from_gray(90)));

    let legend_x = LEGEND_X + 18.0;
    let mut y = LEGEND_Y + 28.0;
    painter.text(
        Pos2::new(legend_x, y),
        Align2::LEFT_CENTER,
        "Method:",
        FontId::proportional(24.0),
        Color32::BLACK,
    );
    y += 32.0;
    painter.text(
        Pos2::new(legend_x + 16.0, y),
        Align2::LEFT_CENTER,
        config.method.label(),
        FontId::proportional(22.0),
        Color32::BLACK,
    );
    y += 42.0;
    painter.text(
        Pos2::new(legend_x, y),
        Align2::LEFT_CENTER,
        "Meals:",
        FontId::proportional(24.0),
        Color32::BLACK,
    );
    painter.circle_filled(
        Pos2::new(legend_x + 52.0, y + 24.0),
        5.0,
        Color32::from_rgb(140, 90, 35),
    );
    painter.circle_stroke(
        Pos2::new(legend_x + 52.0, y + 24.0),
        5.0,
        Stroke::new(1.0, Color32::from_rgb(90, 60, 20)),
    );
    painter.text(
        Pos2::new(legend_x + 70.0, y + 24.0),
        Align2::LEFT_CENTER,
        "= one meal",
        FontId::proportional(20.0),
        Color32::BLACK,
    );
    y += 70.0;
    painter.text(
        Pos2::new(legend_x, y),
        Align2::LEFT_CENTER,
        "Philosophers:",
        FontId::proportional(24.0),
        Color32::BLACK,
    );
    y += 36.0;

    for (label, color) in [
        ("Thinking", Color32::BLUE),
        ("With Right Fork", Color32::from_rgb(235, 155, 45)),
        ("With Left Fork", Color32::from_rgb(150, 80, 200)),
        ("Eating", Color32::GREEN),
        ("Hungry", Color32::RED),
    ] {
        painter.circle_filled(Pos2::new(legend_x + 22.0, y), 20.0, color);
        painter.circle_stroke(
            Pos2::new(legend_x + 22.0, y),
            20.0,
            Stroke::new(1.5, Color32::BLACK),
        );
        painter.text(
            Pos2::new(legend_x + 58.0, y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(18.0),
            Color32::BLACK,
        );
        y += 58.0;
    }

    y += 10.0;
    painter.text(
        Pos2::new(legend_x, y),
        Align2::LEFT_CENTER,
        if config.step_mode {
            "Mode: step-through".to_owned()
        } else {
            format!("Mode: speed {}", config.speed)
        },
        FontId::proportional(18.0),
        Color32::from_rgb(45, 45, 45),
    );
    y += 28.0;
    painter.text(
        Pos2::new(legend_x, y),
        Align2::LEFT_CENTER,
        format!("Counter: {}", counter),
        FontId::proportional(18.0),
        Color32::BLACK,
    );
}

fn phil_color(state: PhilState) -> Color32 {
    match state {
        PhilState::HasNone => Color32::RED,
        PhilState::HasRight => Color32::from_rgb(235, 155, 45),
        PhilState::HasLeft => Color32::from_rgb(150, 80, 200),
        PhilState::HasBoth => Color32::GREEN,
        PhilState::IsFull | PhilState::Thinking => Color32::BLUE,
    }
}

fn state_label(state: PhilState) -> &'static str {
    match state {
        PhilState::HasNone => "Hungry",
        PhilState::HasRight => "Has right",
        PhilState::HasLeft => "Has left",
        PhilState::HasBoth => "Eating",
        PhilState::IsFull => "Full",
        PhilState::Thinking => "Thinking",
    }
}
