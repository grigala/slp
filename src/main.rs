use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS, WindowBuilderExtMacOS};
use tao::window::{Window, WindowBuilder};
use tray_icon::{MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use window_vibrancy::{NSVisualEffectMaterial, NSVisualEffectState, apply_vibrancy};
use wry::{WebView, WebViewBuilder};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const UI_HTML: &str = include_str!("ui.html");
const POPOVER_WIDTH: f64 = 310.0;
const POPOVER_RADIUS: f64 = 12.0;
/// Show a notification this long before the action fires.
const WARN_BEFORE: Duration = Duration::from_secs(60);
const TRAY_ICON: &str = "⏾";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Sleep,
    ShutDown,
    Restart,
}

impl Action {
    fn key(self) -> &'static str {
        match self {
            Action::Sleep => "sleep",
            Action::ShutDown => "shutdown",
            Action::Restart => "restart",
        }
    }

    fn from_key(s: &str) -> Option<Self> {
        [Action::Sleep, Action::ShutDown, Action::Restart]
            .into_iter()
            .find(|a| a.key() == s)
    }

    fn verb(self) -> &'static str {
        match self {
            Action::Sleep => "sleep",
            Action::ShutDown => "shut down",
            Action::Restart => "restart",
        }
    }

    fn perform(self) {
        let result = match self {
            // `pmset sleepnow` works without root.
            Action::Sleep => Command::new("pmset").arg("sleepnow").status(),
            // Ask System Events, the same path the Apple menu uses. Apps get a
            // chance to save documents. macOS asks once for Automation permission.
            Action::ShutDown => osascript(r#"tell application "System Events" to shut down"#),
            Action::Restart => osascript(r#"tell application "System Events" to restart"#),
        };
        if let Err(e) = result {
            eprintln!("slp: failed to {}: {e}", self.verb());
        }
    }
}

fn osascript(script: &str) -> std::io::Result<std::process::ExitStatus> {
    Command::new("osascript").arg("-e").arg(script).status()
}

struct Timer {
    action: Action,
    deadline: SystemTime,
    warned: bool,
}

impl Timer {
    fn remaining(&self) -> Duration {
        self.deadline
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
    }
}

fn notify(message: &str) {
    let script = format!(
        r#"display notification "{}" with title "slp""#,
        message.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let _ = Command::new("osascript").arg("-e").arg(script).spawn();
}

#[derive(Debug)]
enum UserEvent {
    Tray(TrayIconEvent),
    Ipc(String),
}

fn secs_ceil(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

fn fmt_remaining(d: Duration) -> String {
    let s = secs_ceil(d);
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

struct Popover {
    window: Window,
    webview: WebView,
    /// Top-left corner (logical) the popover is pinned to, under the tray icon.
    anchor: LogicalPosition<f64>,
    height: f64,
    /// When the popover was last dismissed by losing focus. Clicking the tray
    /// icon steals focus first, so without this a click would close-then-reopen.
    hidden_at: Option<Instant>,
}

impl Popover {
    fn new(target: &EventLoopWindowTarget<UserEvent>, proxy: EventLoopProxy<UserEvent>) -> Self {
        let height = 560.0;
        let window = WindowBuilder::new()
            .with_title("slp")
            .with_inner_size(LogicalSize::new(POPOVER_WIDTH, height))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_visible(false)
            .with_always_on_top(true)
            .with_visible_on_all_workspaces(true)
            .with_has_shadow(true)
            .build(target)
            .expect("failed to create popover window");

        // Real macOS blur behind the UI, clipped to rounded corners.
        apply_vibrancy(
            &window,
            NSVisualEffectMaterial::HudWindow,
            Some(NSVisualEffectState::Active),
            Some(POPOVER_RADIUS),
        )
            .expect("failed to apply vibrancy");

        let webview = WebViewBuilder::new()
            .with_html(UI_HTML.replace("{{VERSION}}", VERSION))
            .with_transparent(true)
            .with_ipc_handler(move |req| {
                let _ = proxy.send_event(UserEvent::Ipc(req.into_body()));
            })
            .build(&window)
            .expect("failed to create webview");

        Popover {
            window,
            webview,
            anchor: LogicalPosition::new(0.0, 0.0),
            height,
            hidden_at: None,
        }
    }

    fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    fn show_below(&mut self, rect: &tray_icon::Rect) {
        let scale = self.window.scale_factor();
        let icon_pos = rect.position.to_logical::<f64>(scale);
        let icon_size = rect.size.to_logical::<f64>(scale);

        let mut x = icon_pos.x + icon_size.width / 2.0 - POPOVER_WIDTH / 2.0;
        if let Some(m) = self.window.current_monitor().or_else(|| self.window.primary_monitor()) {
            let mpos = m.position().to_logical::<f64>(m.scale_factor());
            let msize = m.size().to_logical::<f64>(m.scale_factor());
            x = x.clamp(mpos.x + 8.0, mpos.x + msize.width - POPOVER_WIDTH - 8.0);
        }
        self.anchor = LogicalPosition::new(x, icon_pos.y + icon_size.height + 4.0);
        self.window.set_outer_position(self.anchor);
        self.window.set_visible(true);
        self.window.set_focus();
    }

    fn hide(&mut self) {
        if self.is_visible() {
            self.window.set_visible(false);
            self.hidden_at = Some(Instant::now());
        }
    }

    fn resize(&mut self, height: f64) {
        if (height - self.height).abs() < 0.5 || height <= 0.0 {
            return;
        }
        self.height = height;
        self.window.set_inner_size(LogicalSize::new(POPOVER_WIDTH, height));
        // AppKit resizes from the bottom-left; keep the top edge pinned.
        if self.is_visible() {
            self.window.set_outer_position(self.anchor);
        }
    }
}

struct App {
    tray: Option<TrayIcon>,
    popover: Option<Popover>,
    selected: Action,
    timer: Option<Timer>,
}

impl App {
    fn start(&mut self, d: Duration) {
        self.timer = Some(Timer {
            action: self.selected,
            deadline: SystemTime::now() + d,
            warned: d <= WARN_BEFORE,
        });
        self.refresh();
    }

    fn extend(&mut self, d: Duration) {
        if let Some(t) = &mut self.timer {
            t.deadline += d;
            if t.remaining() > WARN_BEFORE {
                t.warned = false;
            }
        }
        self.refresh();
    }

    fn select_action(&mut self, action: Action) {
        self.selected = action;
        // Changing the action also retargets a running timer.
        if let Some(t) = &mut self.timer {
            t.action = action;
        }
        self.refresh();
    }

    fn on_tray(&mut self, event: TrayIconEvent) {
        let TrayIconEvent::Click { rect, button_state: MouseButtonState::Down, .. } = event else {
            return;
        };
        let Some(p) = &mut self.popover else { return };
        if p.is_visible() {
            p.hide();
        } else if p.hidden_at.is_none_or(|t| t.elapsed() > Duration::from_millis(300)) {
            p.show_below(&rect);
            self.refresh();
        }
    }

    /// Handle a message from the web UI. Returns `false` to quit.
    fn on_ipc(&mut self, body: &str) -> bool {
        let Ok(msg) = serde_json::from_str::<Value>(body) else { return true };
        let minutes = || msg["minutes"].as_f64().filter(|m| *m > 0.0 && *m <= 24.0 * 60.0);
        match msg["cmd"].as_str().unwrap_or_default() {
            "start" => {
                if let Some(m) = minutes() {
                    self.start(Duration::from_secs((m * 60.0).round() as u64));
                }
            }
            "extend" => {
                if let Some(m) = minutes() {
                    self.extend(Duration::from_secs((m * 60.0).round() as u64));
                }
            }
            "action" => {
                if let Some(a) = msg["value"].as_str().and_then(Action::from_key) {
                    self.select_action(a);
                }
            }
            "cancel" => {
                self.timer = None;
                self.refresh();
            }
            "resize" => {
                if let (Some(p), Some(h)) = (&mut self.popover, msg["height"].as_f64()) {
                    p.resize(h);
                }
            }
            "hide" => {
                if let Some(p) = &mut self.popover {
                    p.hide();
                }
            }
            "quit" => return false,
            _ => {}
        }
        true
    }

    /// Called twice a second. Fires the action when the deadline passes.
    fn tick(&mut self) {
        if let Some(t) = &mut self.timer {
            let remaining = t.remaining();
            if remaining.is_zero() {
                let action = t.action;
                self.timer = None;
                if let Some(p) = &mut self.popover {
                    p.hide();
                }
                self.refresh();
                action.perform();
                return;
            }
            if !t.warned && remaining <= WARN_BEFORE {
                t.warned = true;
                notify(&format!(
                    "Your Mac will {} in 1 minute. Click ⏾ in the menu bar to cancel or extend.",
                    t.action.verb()
                ));
            }
        }
        self.refresh();
    }

    /// Push the current state to the menu-bar title and the popover UI.
    fn refresh(&self) {
        let title = match &self.timer {
            Some(t) => format!("{TRAY_ICON} {}", fmt_remaining(t.remaining())),
            None => TRAY_ICON.to_string(),
        };
        if let Some(tray) = &self.tray {
            tray.set_title(Some(title));
        }

        let Some(p) = &self.popover else { return };
        if !p.is_visible() {
            return;
        }
        let state = json!({
            "action": self.timer.as_ref().map_or(self.selected, |t| t.action).key(),
            "running": self.timer.is_some(),
            "remaining": self.timer.as_ref().map_or(0, |t| secs_ceil(t.remaining())),
        });
        let _ = p.webview.evaluate_script(&format!("window.update && window.update({state})"));
    }
}

fn main() {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // Menu-bar only: no Dock icon, no app switcher entry.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Tray(e));
    }));
    let ipc_proxy = event_loop.create_proxy();

    let mut app = App { tray: None, popover: None, selected: Action::Sleep, timer: None };
    let mut next_tick = Instant::now();

    event_loop.run(move |event, target, control_flow| {
        let now = Instant::now();
        if now >= next_tick {
            app.tick();
            next_tick = now + Duration::from_millis(500);
        }
        *control_flow = ControlFlow::WaitUntil(next_tick);

        match event {
            // On macOS the status item must be created once the loop is running.
            Event::NewEvents(StartCause::Init) => {
                app.tray = Some(
                    TrayIconBuilder::new()
                        .with_title(TRAY_ICON)
                        .with_tooltip("slp — sleep timer")
                        .build()
                        .expect("failed to create menu bar item"),
                );
                app.popover = Some(Popover::new(target, ipc_proxy.clone()));
                app.refresh();
            }
            Event::WindowEvent { event: WindowEvent::Focused(false), .. } => {
                if let Some(p) = &mut app.popover {
                    p.hide();
                }
            }
            Event::UserEvent(UserEvent::Tray(e)) => app.on_tray(e),
            Event::UserEvent(UserEvent::Ipc(body)) if !app.on_ipc(&body) => {
                app.popover.take();
                app.tray.take();
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
