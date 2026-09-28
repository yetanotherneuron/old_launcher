---
name: iced
description: Compose and structure iced (iced-rs) GUI applications in Rust. Workspace layout, TEA composition with Action<I, M> + .map() bubbling, iced API conventions (function helpers over Widget::new, keyboard listen/filter_map, widget composition), and style. Use when starting an iced project, adding a screen, refactoring an iced state machine, or reviewing iced code that doesn't compose cleanly.
---

# /iced

Conventions for writing iced GUI code that composes. Four parts: **workspace**,
**TEA composition**, **iced API**, **Rust style**. Apply together — each
reinforces the others.

The canonical reference for raw Rust style is `guides/RUST_STYLE.md`. The
`guides/` folder sits beside the `skills/` folder this skill was installed
into (`~/.claude/guides`, `~/.codex/guides`, or your own agent's home).
This skill is the iced-specific addendum + idioms.

---

## 1. Workspace structure

### When you need a workspace at all

A single-binary throwaway can live in one crate. The moment you have:
- pure data types you'd want to test without iced in the way, or
- domain logic that's worth testing or reusing (a parser, a scheduler, a diff algorithm), or
- a rendering primitive that could outlive this app (a custom widget library, a charting layer), or
- a second binary (TUI, CLI tool, server-side variant)

…it's worth splitting into a workspace. The threshold is "would I be embarrassed
to have iced in a unit test that doesn't render anything?"

### The default layer split

```
your-app/
├── Cargo.toml           [workspace] + root [package] (the bin)
├── src/
│   ├── main.rs          thin: iced::application(...).run()
│   ├── lib.rs           top-level `App` struct, Screen enum, Message enum
│   └── auth.rs, dashboard.rs, …    the screens — model-and-UI lives here
└── core/                pure data + logic — no iced, no IO
    ├── Cargo.toml
    └── src/lib.rs
```

Two crates is the common case. The **root bin** (`src/`) holds everything that
is model-and-UI — the `App`, the screens, their `State`/`Message`/`Instruction`,
the view code. **`core`** holds the pure data types and the logic over them that
you'd want to test without iced in the way (a parser, a query planner, the
domain state machine and the rules that validate it).

There is **no `ui` crate by default.** View code is not a layer you
extract — it lives next to the screen it draws, in `src/`. A dedicated `ui`
crate earns its place only in *very large* apps, and only for **generic,
app-agnostic** widgets and components — a reusable design-system layer that
knows nothing about your screens. That's overkill 99% of the time; reach for it
only when the generic-widget surface is large enough to test and reuse on its
own.

The one hard rule: **never put iced types in `core`.** It's the testable core;
iced is a presentation concern that lives in the bin.

When a further crate is justified — an asset pipeline, or wire-protocol types
shared with a server binary — add a sibling crate (`assets`, `protocol`). It
stays content-agnostic: it knows nothing about your specific app's screens or
domain.

### Member crate naming

Default to short, **unprefixed** names: a `model` / `assets` / `protocol` crate
is just that. The crate name is its identity inside the workspace; the project
name lives at the root. Don't reflexively prefix everything with the app name
(`your-app-model`) — these aren't published to crates.io, so there's no global
namespace to dodge.

**Prefix only when the bare name actually collides** — and keep the directory
short either way (Cargo doesn't require the directory and the package name to
match). The canonical case is `core`: a package literally named `core` clashes
with the standard library's `core` crate, so the *package* becomes
`your-app-core` while its *directory* stays `core/`.

When you do prefix, **reexport the subcrate under your app's namespace** so call
sites read `your_app::core`, never `your_app_core` — exactly how iced surfaces
its own `iced_core` / `iced_widget` packages (short dirs `core/`, `widget/`) as
`iced::core` / `iced::widget`:

```rust
// src/lib.rs — the app's facade
pub use your_app_core as core;   // now everything is `crate::core::Thing`
```

(This is a public reexport that *establishes* the canonical name — distinct from
the call-site `use foo as bar` aliasing that's banned below.)

### Workspace boilerplate

```toml
# root Cargo.toml — the bin package and the [workspace] table share one file.

[package]
name = "your-app"
version.workspace = true
edition.workspace = true

[dependencies]
your-app-core = { workspace = true }
iced = { workspace = true }
tokio = { workspace = true }

[workspace]
resolver = "3"
members = ["core"]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
# short directory `core/`, prefixed package (bare `core` collides with std)
your-app-core = { path = "core" }

# iced 0.15 is unreleased — track it from git and pin features once here.
iced = { version = "0.15.0-dev", features = ["tokio", "image", "markdown"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
thiserror = "2"

# Replace the crates.io iced with the dev branch. Patch every iced crate that
# actually lands in your tree from the SAME git source — here just the umbrella;
# add iced_core / iced_widget if a member depends on them directly.
[patch.crates-io]
iced = { git = "https://github.com/iced-rs/iced", branch = "master" }
```

The `core/` member is a plain library crate, deliberately free of iced:

```toml
# core/Cargo.toml
[package]
name = "your-app-core"
version.workspace = true
edition.workspace = true

[dependencies]
serde = { workspace = true }
```

Member crates pull shared deps from the workspace with `iced = { workspace = true }`.

---

## 2. TEA composition — the canard / receipts pattern

iced apps are pure Elm Architecture: `State + Message + update + view`. The
question is HOW to scale this across multiple screens / panels / sub-flows
without ending up with one 2000-line `Message` enum and one giant `update`
match.

The pattern below is what canard and receipts (two well-composed iced
codebases) converge on independently. Adopt it from the start; retrofitting
later is painful.

### Each screen module is a self-contained TEA cell

```rust
// src/auth.rs

use iced::{Element, Subscription, Task};
use crate::action::Action;

pub struct State {
    server_url: String,
    email: String,
    password: String,
    error: Option<String>,
    stage: Stage,
}

enum Stage { Idle, Submitting, WaitingForOAuth { otp: [u8; 32] } }

#[derive(Debug, Clone)]
pub enum Message {
    ServerUrlChanged(String),
    EmailChanged(String),
    PasswordChanged(String),
    Submit,
    AuthResult(Result<Account, AuthError>),
}

pub enum Instruction {
    Authenticated(Account),
    Cancelled,
}

impl State {
    pub fn new() -> Self { ... }

    pub fn update(&mut self, message: Message) -> Action<Instruction, Message> {
        match message {
            Message::Submit => Action::task(Task::perform(submit(...), Message::AuthResult)),
            Message::AuthResult(Ok(account)) => Action::instruction(Instruction::Authenticated(account)),
            Message::AuthResult(Err(err)) => { self.error = Some(err.to_string()); Action::none() }
            // ...
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        // returns Element typed against THIS screen's Message, not the parent's
        column![...].into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        keyboard::listen().filter_map(...)
    }
}
```

The screen module exports its **own** `Message`, `Instruction`, `State`,
`update`, `view`, `subscription`. The parent never sees the screen's internal
Message variants directly.

### The `Action<I, M>` abstraction

```rust
// src/action.rs

use iced::Task;

#[must_use = "Action.task must be returned to iced; Action.instruction must be matched by the parent"]
pub struct Action<I, M> {
    pub instruction: Option<I>,
    pub task: Task<M>,
}

impl<I, M> Action<I, M> {
    pub fn none() -> Self {
        Self { instruction: None, task: Task::none() }
    }

    pub fn instruction(i: I) -> Self {
        Self { instruction: Some(i), task: Task::none() }
    }

    pub fn task(t: Task<M>) -> Self {
        Self { instruction: None, task: t }
    }

    pub fn new(i: I, t: Task<M>) -> Self {
        Self { instruction: Some(i), task: t }
    }

    pub fn map<N>(self, f: impl Fn(M) -> N + Send + 'static) -> Action<I, N>
    where M: Send + 'static, N: Send + 'static
    {
        Action { instruction: self.instruction, task: self.task.map(f) }
    }

    pub fn map_instruction<N>(self, f: impl Fn(I) -> N + Send + 'static) -> Action<N, M>
    where I: Send + 'static, N: Send + 'static
    {
        Action { instruction: self.instruction.map(f), task: self.task }
    }

    pub fn with_task(mut self, t: Task<M>) -> Self { self.task = t; self }
    pub fn with_instruction(mut self, i: I) -> Self { self.instruction = Some(i); self }
}

impl<I, M> From<Task<M>> for Action<I, M> {
    fn from(t: Task<M>) -> Self { Self::task(t) }
}
```

The `#[must_use]` is load-bearing: it catches dropped tasks and instructions at
compile time. Keep it.

`map_instruction` is the rare one — reach for it **only** to pass an instruction
straight through a layer unchanged (a parent re-bubbling a child's Instruction
to *its* parent without acting on it). Most parents match the instruction in
place and drive a transition, so `map_instruction` should be uncommon in app
code. Use it sparingly.

Two reasons this beats a plain enum:
- **Composable**: you can both fire a task AND signal an instruction (e.g.
  "authenticated AND focus the next field") with `Action::new(i, t)` or
  `Action::instruction(i).with_task(t)`.
- **Mappable**: parent can transform child's Message and Instruction types
  independently before bubbling.

A simpler enum-based version works for small apps; the struct version pays off
the moment you have a third screen and start needing chained transformations.

### Parent composition via `.map()`

```rust
// src/lib.rs

use iced::{Element, Subscription, Task};
use crate::auth;
use crate::dashboard;

pub enum Screen {
    Auth(auth::State),
    Dashboard(dashboard::State),
}

pub struct App {
    screen: Screen,
    // app-wide state (not screen-local) goes here
}

#[derive(Debug, Clone)]
pub enum Message {
    Auth(auth::Message),
    Dashboard(dashboard::Message),
    // app-wide messages
}

impl App {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        // Keep the per-screen handling inline in the match arm. Don't extract a
        // `handle_auth` method — see "Don't extract single-use helpers" below.
        match message {
            Message::Auth(message) => {
                let Screen::Auth(auth) = &mut self.screen else {
                    return Task::none();
                };
                let action = auth.update(message);

                // React to instructions BEFORE returning the mapped task
                if let Some(instruction) = action.instruction {
                    match instruction {
                        auth::Instruction::Authenticated(acct) => {
                            self.screen =
                                Screen::Dashboard(dashboard::State::new(acct));
                        }
                        auth::Instruction::Cancelled => { /* ... */ }
                    }
                }

                action.task.map(Message::Auth)
            }
            Message::Dashboard(message) => {
                // same shape: destructure the screen, update, react to its
                // instructions in place, then `action.task.map(Message::Dashboard)`
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        match &self.screen {
            Screen::Auth(s) => s.view().map(Message::Auth),
            Screen::Dashboard(s) => s.view().map(Message::Dashboard),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        match &self.screen {
            Screen::Auth(s) => s.subscription().map(Message::Auth),
            Screen::Dashboard(s) => s.subscription().map(Message::Dashboard),
        }
    }
}
```

The pattern is mechanical:
- `child.view().map(Message::Variant)` — for views
- `child.subscription().map(Message::Variant)` — for subscriptions
- `action.task.map(Message::Variant)` — for the runtime task
- `action.instruction` — matched in place to drive parent-level transitions

### Routing to one of many children of the same type

The `Screen` enum above has one variant per *distinct* child type. When the
parent owns *many children of the same type* — todos in a list, tabs in a tab
bar, tiles in a grid — wrap the child Message in an id-tagged envelope:

```rust
pub enum Message {
    Todo { id: todo::Id, message: todo::Message },   // struct-style (preferred)
    // or tuple-style — same shape, terser:
    // Todo(todo::Id, todo::Message),
    AddTodo,
    RemoveTodo(todo::Id),
}
```

The parent's update routes by id and re-tags the child's task on the way out:

```rust
fn update(&mut self, message: Message) -> Task<Message> {
    match message {
        Message::Todo { id, message } => {
            let Some(todo) = self.todos.get_mut(&id) else { return Task::none() };
            let action = todo.update(message);
            // ... handle action.instruction ...
            action.task.map(move |message| Message::Todo { id, message })
        }
        Message::AddTodo => { ... }
        Message::RemoveTodo(id) => { ... }
    }
}
```

**Protip — `.with()` on tuple variants.** iced ships an `iced::Function` trait
(`use iced::Function;`, or a project-local re-export) that adds a `.with(prefix)`
combinator to binary function values. Tuple-style enum variants ARE binary
functions — `Message::Todo` has signature `fn(todo::Id, todo::Message) -> Message`
— so they pick up `.with()` for free:

```rust
// tuple-style variant: Todo(todo::Id, todo::Message)
action.task.map(Message::Todo.with(id))   // same as the closure above
```

Struct variants don't have this shape (they're not callable as functions), so
they still need the explicit `move |message| Message::Todo { id, message }`
closure. Both compose fine; pick whichever reads better at the call site.

A few rules that keep this clean:

- **The id lives at the parent, not the child.** The child knows it's `a todo`,
  not `the todo at id=42`. Keep the BTreeMap key (or Vec index, or whatever)
  on the parent side and supply it on entry/exit.
- **The `.map(move |message| Message::Todo { id, message })` captures the id by
  move.** Async tasks the child kicks off may emit follow-up messages that need
  to route back to the same child — the move closure ensures they do.
- **Fan-out is the parent's job, not the grandparent's.** When something needs
  to reach every child, the parent builds the Task::batch over its own
  collection:

  ```rust
  fn broadcast_tick(&self) -> Task<Message> {
      let tasks: Vec<_> = self.todos.keys().map(|&id| {
          Task::done(Message::Todo { id, message: todo::Message::Tick })
      }).collect();
      Task::batch(tasks)
  }
  ```

  The grandparent `.map(Message::Parent)`s this once. **Grandparents must
  never construct grandchild-addressed envelopes directly** — that bypasses
  the `.map()` chain and is the most common shape of "imperative TEA" rot.
  If a grandparent finds itself walking a grandchild's collection, that walk
  belongs as a Message on the parent.

### When to use `Instruction` vs returning Task directly

| | Use Instruction | Use Task |
|---|---|---|
| Cross-screen navigation | yes | no |
| Mutating app-level state (not screen-local) | yes | no |
| Async work (HTTP, file IO, timer) | no | yes |
| Iced widget operations (focus, scroll) | no | yes (operation::focus_next() etc) |
| Both at once | `Action::new(i, t)` or `instruction(i).with_task(t)` | n/a |

If a child screen needs to navigate but ALSO kick off a fetch, it's two effects
in one Action — return `Action::new(instruction, task)`. The parent matches the
instruction, then returns the task.

### Subscription composition

Subscriptions follow the same `.map()` pattern. Multiple subscriptions per
screen combine with `Subscription::batch`:

```rust
pub fn subscription(&self) -> Subscription<Message> {
    Subscription::batch([
        keyboard::listen().filter_map(handle_key),
        time::every(milliseconds(100)).map(|_| Message::Tick),
    ])
}
```

Parent composes per-screen subscriptions the same way as views/tasks.

### Inner function signatures are free-form

**Only the top-level `update` and `view` functions** (the ones passed to
`iced::application(state, update, view).run()`) have fixed signatures. Inner
screen modules can take whatever signatures their composition needs:

```rust
// All of these are legal screen-level update signatures.

pub fn update(&mut self, message: Message);  // returns ()
pub fn update(&mut self, message: Message) -> Task<Message>;
pub fn update(&mut self, message: Message) -> Action<Instruction, Message>;
pub fn update(&mut self, message: Message, client: &Client) -> Action<...>;
pub fn update(state: &mut MyState, message: Message, db: &Db, now: Instant) -> Action<...>;
// (the last form is "free function with state arg" — receipts uses this)
```

Take whatever read-only dependencies you need, **each as its own named
argument** — an immutable `&Client` for network calls, a `&Db`, a clock. Pass
several when you need several. Don't bundle them into a god `Context` /
`AppContext` struct just to thread one parameter; a grab-bag "Context" type is a
big smell to avoid. Add a newtype only when the bundle is intrinsically a
meaningful thing on its own, never as a passing convenience. Return whatever
shape makes sense for the parent.

### TEA gives you "free replays"

A consequence of pure TEA: a sequence of Messages applied to an initial state
produces a deterministic final state. Three places this pays off:

1. **Unit tests** — apply a Message sequence, assert end state. No iced widgets
   in test code; you're testing the logic, not the renderer.
2. **Stable mid-state previews** — an `at_mid_state()` builder that constructs
   state via replay with a fixed RNG seed, so a preview of some deep-in-the-flow
   screen reproduces identically across rebuilds.
3. **Save/load + bug repros** — persist the action log, not the snapshot.
   A megabyte of state becomes a kilobyte of actions.

If you have RNG, seed it explicitly. Determinism dies the moment you reach for
`thread_rng()`.

```rust
// Replay helper that any TEA app benefits from
pub fn replay(mut state: State, rng: &mut Rng, steps: &[Step]) -> State {
    for step in steps {
        if state.is_resolved() { break; }
        let _ = state.apply(step, rng);
    }
    state
}
```

### Push state down — scaling Instructions

When a child's Instruction enum grows and the parent mechanically sets
fields on its behalf, push `&mut` handles down so the child mutates
directly. Instructions shrink to policy-only (navigation, server sync,
pool provisioning). Single-use `route_*` helpers vanish.

Full guide: `guides/PUSH_STATE_DOWN.md` — covers policy vs
plumbing classification, blanket-sweep smell, minimal `&mut self`
surface, and the checklist.

---

## 3. iced API conventions

iced's surface is large. These are the patterns that come up constantly and
have a "right" answer most projects miss.

### Function helpers over `::new()`

iced's widgets all have function-helper constructors. **Use them.** The
`Widget::new(...)` form is legacy / internal-feeling.

```rust
// Bad
let s = Space::new(Length::Fill, Length::Shrink);
let b = Button::new(Text::new("submit"));
let c = Container::new(col).padding(20);

// Good
let s = space::horizontal();          // Length::Fill width by default
let b = button(text("submit"));
let c = container(col).padding(20);
```

Catalog (iced ≥0.13):

| Helper | Replaces | Notes |
|---|---|---|
| `text("…")` | `Text::new("…")` | takes anything `Into<String>` |
| `button(content)` | `Button::new(content)` | adds `.on_press(message)` for activation |
| `container(content)` | `Container::new(content)` | wraps anything |
| `column![a, b, c]` macro | `Column::new().push(a).push(b)...` | also `column(iterator)` for dynamic |
| `row![a, b, c]` macro | `Row::new()...` | same |
| `space::vertical()` / `space::horizontal()` | `Space::new(...)` | with `.width()` / `.height()` overrides |
| `image(path_or_handle)` | `Image::new(...)` | feature-gated |
| `text_input(placeholder, value)` | `TextInput::new(...)` | |
| `checkbox(value)` | `Checkbox::new(...)` | `.label("…").on_toggle(message)` |
| `slider(range, value, on_change)` | `Slider::new(...)` | |
| `pick_list(options, current, on_select)` | `PickList::new(...)` | |
| `scrollable(content)` | `Scrollable::new(...)` | |
| `stack![a, b, c]` macro | `Stack::new()...` | layer widgets z-order |

Plus alignment shorthands:

| Shorthand | Replaces |
|---|---|
| `center(content)` | `container(content).center_x(Fill).center_y(Fill)` |
| `iced::Center` (const) | `Alignment::Center` literal |

### Keyboard subscriptions

Use `keyboard::listen().filter_map(...)`. **`on_key_press` does not exist**
in current iced. The listen-then-filter pattern is canonical:

```rust
use iced::keyboard::{self, key::Named, Key};

keyboard::listen().filter_map(|event| match event {
    keyboard::Event::KeyPressed { key, modifiers, .. } => match key {
        Key::Named(Named::Enter) => Some(Message::Submit),
        Key::Named(Named::Escape) => Some(Message::Cancel),
        Key::Character(c) if c.as_str() == "?" => Some(Message::Help),
        _ => None,
    },
    _ => None,
})
```

For arrow keys, function keys, tabs — use `Key::Named(Named::ArrowLeft)`, etc.
For character input — `Key::Character(c)` where `c` is `SmolStr` (compare via
`c.as_str()`).

### Layout helpers

| Type | Meaning |
|---|---|
| `Length::Fill` | take all available space along this axis |
| `Length::FillPortion(n)` | take `n/total` of available space, where total is the sum of FillPortions on this axis |
| `Length::Shrink` | take only what content needs |
| `Length::Fixed(px)` | exactly N pixels |
| `iced::Fill` (const) | `Length::Fill` shorthand |

For padding, use the `padding!` macro or `.padding(n)` / `.padding([y, x])`
on most widgets.

### Theme + style closures

```rust
container(content)
    .style(|_theme: &Theme| container::Style {
        background: Some(MY_COLOR.into()),
        text_color: Some(Color::WHITE),
        border: iced::Border { color: GOLD, width: 1.0, radius: 4.0.into() },
        ..Default::default()
    })
```

Style closures take `&Theme` and return a `Style`. They run on every layout
pass — keep them cheap; precompute colors as `const`.

### `Task` building blocks

```rust
Task::none()                              // no-op
Task::done(message)                       // synchronous result
Task::perform(future, |result| Message::X(result))   // async
Task::batch([a, b, c])                    // run in parallel
a.chain(b)                                // run sequentially
task.map(Message::Inner)                  // pass the variant itself — not |message| Message::Inner(message)
```

For widget-level operations (focus a field, scroll a container) use the
`iced::widget::operation` module:

```rust
Task::done(operation::focus_next())
Task::done(operation::scroll_to(scrollable_id, offset))
```

### Color construction

**Reach for `iced::color!(0xRRGGBB)`.** It's the first-class way to write a
color and the right choice 99% of the time — everything else is a hack by
comparison.

```rust
let accent = color!(0xd4a64a);
let translucent = color!(0xd4a64a, 0.5);   // optional alpha
```

The one thing `color!` can't do is appear in a `const`: it expands to a runtime
`debug_assert!`, so it isn't a const fn. For a `pub const` color, use one of
`Color`'s own const constructors — `from_packed_rgb8(0xRRGGBB)` reads almost like
`color!`:

```rust
const ACCENT: Color = Color::from_packed_rgb8(0xd4a64a);
```

(`from_rgb8(r, g, b)`, `from_rgba8`, and the `f32`-based `from_rgb` / `from_rgba`
are all `const` too.) Reserve these for the const case — anywhere you're writing
a literal color in normal code, `color!` is the answer.

### Boot pattern (constructor returning state + initial task)

```rust
impl App {
    pub fn new() -> (Self, Task<Message>) {
        let state = Self { /* ... */ };
        let initial_task = Task::perform(load_config(), Message::ConfigLoaded);
        (state, initial_task)
    }
}

// in main.rs
iced::application(App::new, App::update, App::view).run()
```

iced's `BootFn` impls cover both `() -> Self` and `() -> (Self, Task<Message>)`,
so either works. Use the tuple form whenever the app needs to kick off async
work at startup.

### Stack widget for layering

```rust
use iced::widget::stack;

stack![
    content,                                    // back layer
    container(toast).padding(20),               // front overlay
]
```

Use `stack!` when you want one widget visually on top of another — a modal over
a form, a toast over the content, a floating toolbar over a scrollable.

### Common iced 0.15-dev gotchas

- `image`, `svg`, `qr_code`, `markdown` are feature-gated. Add the
  feature to your iced dependency or imports fail.
- Some widget builder methods take `impl Into<Length>` and accept bare integers
  via the `From<u16> for Length` impl — `.height(8)` works, treated as
  `Length::Fixed(8.0)`.
- `Element<'a, Message>` lifetime is tied to whatever widgets borrow from in
  the `view` body. Don't try to construct an `Element<'static>` and store it
  in `State` — iced rebuilds the element tree every render. Build elements
  inside `view`; let lifetimes flow naturally.
- `Text` widgets' size is in points (logical pixels), not pixels. Default font
  size is 16; size-20 text is noticeably larger.

---

## 4. Rust style for iced

Apply `guides/RUST_STYLE.md` throughout. Key rules that hit
iced code especially hard:

### Don't extract single-use helpers — minimize API surface

Two long functions you can read top to bottom are easier to follow than twelve
short ones you have to cross-reference. Every extracted function or method is
another name, another signature, another call edge to hold in your head — API
surface is a cost, and the cost compounds: understanding how 100 helpers
interact is far harder than reading two long bodies straight through.

So **keep logic inline at its one call site.** The parent `update` above handles
each screen directly in its match arm — no `handle_auth` / `handle_dashboard`
methods, because each would be called exactly once. Extracting it doesn't
simplify anything; it just scatters one readable flow across the file and forces
the reader to jump.

The bar for pulling something out is **demonstrated, repeated** use — and not
even always then:

- **Never extract before duplication exists.** Don't write a helper "because
  I'll probably need it again." Speculative helpers are pure surface with no
  payoff. Wait for the second (often third) real call site.
- **Even repeated code can stay duplicated.** Two similar blocks that drift
  apart over time — different validation, different edge cases — are *clearer*
  duplicated than fused behind a helper bristling with flags and `Option` params
  to paper over their differences. DRY is overrated.
- **We want OPAQUE, not DRY.** Extract to seal a genuine abstraction behind a
  clean boundary (a well-named type/operation that hides real complexity and you
  can reason about without reading its body), not merely to delete repeated
  characters. If the "helper" is just "these 8 lines, but named," leave the 8
  lines where they are.

**View code is the exception — and the exception is real.** Everything above is
about *logic* (`update`, async wiring, state transitions): keep it inline.
`view` is different. Composing a view out of separate `-> Element<'_, Message>`
fragments is genuinely helpful *and* the iced idiom — an `Element` is a value,
and building it from smaller value-returning functions reads better than one
monolithic `view`, even when a fragment is used once. So for views, treat
extraction as a ladder you climb only as far as the app actually needs:

- **Level 0 — everything inline in one `fn view`.** The right default for a
  small screen. No fragments.
- **Level 1 — extract a semantically meaningful chunk as a function.** When part
  of a screen is itself composed of smaller pieces and reads as *a thing* — a
  card, a toolbar, a summary panel — give it a `fn card(...) -> Element<'_,
  Message>`. The win is comprehension: a `view` that reads `column![header(…),
  card(…), footer(…)]` is easier to parse than the same 80 lines inlined. Single
  use is fine here — unlike a logic helper, a view fragment doesn't need to
  repeat *in code* to earn its name; it usually corresponds to something that
  repeats *on screen* or bundles many widgets into one nameable unit. Still be
  judicious — don't spam a function per `column`/`row` (and don't shadow iced's
  own `row`/`column` helpers). Extracting for layout alone buys nothing; extract
  when a name makes the view easier to understand. Don't default to it; reach
  for it when inlining stops reading cleanly.
- **Level 2 — reusable components generic over `Message`.** Pull a fragment up
  into a function that's generic over the message it emits and takes its data as
  parameters ("props", to borrow a term iced doesn't use), returning an
  `Element<'a, Message>` you can drop into many screens.
- **Level 3 — components as their own `Newtype<'a>` with a builder API.** When a
  component grows real structure, give it a type: `struct Card<'a, Message>`
  holding its data and children, with builder methods (`.footer(...)`, a rounded
  border applied to every child, a typed `submit: Option<Button<'a, Message>>`
  plus a `&str` label rather than a type-erased `Element`). Now it has an API,
  not just a signature.
- **Level 4 — promote Level 3 into a dedicated `ui` crate.** Only for a massive
  app (this is the "very large apps only" `ui` crate from §1). In practice
  almost never — a thought for a future big project, not a starting point.

Climb only when the current level hurts. Most screens live at Level 0–1; a
mature app reaches Level 2–3 for its handful of recurring components.

### Module structure — no `mod.rs`

```
src/
├── lib.rs
├── auth.rs          # auth module — no children
├── dashboard.rs     # dashboard module
└── dashboard/       # dashboard's children
    ├── widget.rs
    └── status.rs
```

Never `dashboard/mod.rs`. The `foo.rs + foo/` pattern keeps every editor tab
named meaningfully.

### One semantic type per module

Each screen lives in its own file. Don't put `Auth`, `Dashboard`, `Settings`
all in `src/lib.rs` — each gets a file, and `lib.rs` is just the parent's
composition layer + module declarations.

> **CRITICAL — "one type" means one *primary* type plus the satellites that
> exist only to serve it, not literally one `pub` item per file.** A screen
> module is the textbook case: its `State`, `Message`, `Instruction`, and any
> `Stage` / `Kind` sub-enums are ONE semantic unit — the TEA cell. They are not
> separate concerns that each deserve their own file; `Message` and `Instruction`
> have no meaning apart from *this* screen's `State`. **Do not** scatter them
> into `auth/state.rs`, `auth/message.rs`, `auth/instruction.rs` — keep the whole
> cell in `auth.rs`.
>
> The test (straight from `RUST_STYLE.md`): a type earns its own module only if
> code that doesn't care about the parent could plausibly import it on its own.
> `auth::State` and `auth::Message` are always imported together as a pair, so
> they share a module. Two *unrelated* primary types — `Auth` and `Dashboard` —
> never do. Splitting the cell buys nothing and forces the reader to chase three
> files to understand one screen.

### No composite names — use the module path

This bites iced code constantly because Message + State + Instruction are the
SAME shape in every screen module. Use the module:

```rust
// Bad
pub struct AuthState { ... }
pub enum AuthMessage { ... }
pub enum AuthInstruction { ... }

// Good
// Inside `pub mod auth`:
pub struct State { ... }
pub enum Message { ... }
pub enum Instruction { ... }

// Call site disambiguates by path:
let action: Action<auth::Instruction, auth::Message> = auth_state.update(message);
```

The same applies inside a single module — if you have an enum with a `Kind`
sub-discriminator, name it `Kind` in a sub-module, not `FooKind`:

```rust
pub mod status {
    pub enum Kind { Active, Pending, Closed }
}
pub struct Status {
    pub kind: status::Kind,
    pub since: Instant,
}
```

Single-word names that *aren't* composite stay at the parent level: `Status`,
`Verb`, `Target`, `Outcome`, `Phase`. The rule is "drop the prefix that's
already supplied by the module path."

### No aliased imports

```rust
// Bad
use crate::auth::Message as AuthMessage;
use crate::dashboard::Message as DashboardMessage;

// Good
use crate::auth;
use crate::dashboard;

fn handle(am: auth::Message, dm: dashboard::Message) { ... }
```

**`use foo as bar` is banned — no exceptions, including name collisions.** When
a module in your bin crate collides with one in `core` — say `src/media.rs` (a
screen) and `core::media` (the domain types) — do **not** rename the import.
Re-export what you need from `core` *into* the local module instead:

```rust
// src/media.rs
pub use core::media::Film;     // surface the domain type through this module

pub struct Screen { ... }      // the app-local, UI-side type
pub enum Message { ... }
```

Now every sibling in the bin crate writes `use crate::media;` and refers to
`media::Film` (the domain type) and `media::Screen` (the app type) through one
path, transparently. The local module is the single seam where the two `media`s
meet — and no alias ever leaks to a call site.

### Import the parent module when using >1 item

```rust
// Good
use crate::resource;
fn check(s: resource::Schema, r: resource::Registry) { ... }

// Also fine — when you use exactly one thing
use crate::resource::Schema;
fn check(s: &Schema) { ... }

// Bad
use crate::resource::*;
```

### Error handling

- Library crates use `thiserror`:
  ```rust
  #[derive(Debug, thiserror::Error)]
  pub enum Error {
      #[error("validation failed: {0}")]
      Validation(String),
      // ...
  }
  ```
- Binary crates use `anyhow::Result` for top-level error handling.
- **No `unwrap()` in library code.** Use `expect("reason: invariant ...")` for
  invariants, or propagate with `?`. Tests can `unwrap` freely.

### Code organization within a file

```rust
//! 1. Module-level doc comment

// 2. Imports (std, external, crate-internal — blank lines between)
use std::collections::VecDeque;
use iced::Element;
use crate::action::Action;

// 3. Type definitions
pub struct State { ... }
pub enum Message { ... }
pub enum Instruction { ... }

// 4. Trait implementations
impl Default for State { ... }
impl From<X> for Message { ... }

// 5. Inherent implementations
impl State {
    pub fn new() -> Self { ... }
    pub fn update(&mut self, message: Message) -> Action<Instruction, Message> { ... }
    pub fn view(&self) -> Element<'_, Message> { ... }
}

// 6. Private helpers
fn build_input<'a>(label: &'a str, value: &'a str) -> Element<'a, Message> { ... }

// 7. Tests
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_with_empty_email_errors() { ... }
}
```

### Comments & docs

Doc every public item (`#![warn(missing_docs)]` is on in most of these crates),
and write comments to the bar in `RUST_STYLE.md` → **Documentation**: explain the
*why* in the present tense, and cut chain-of-thought narration, `// Phase 2`-style
plan bookkeeping, and archaeology (`// was previously inlined`, `// changed to a
HashMap`). The code reads as though it was always this way.

### Edition + dependencies

Rust 2024 edition. Conservative deps — prefer std, then well-known crates
(serde, tokio, thiserror, anyhow, tracing). Add to the allowlist deliberately.

---

## 5. Testing iced code

Two kinds of tests, and they look nothing alike. Which one you write depends on
which crate the code lives in.

### `core` logic — plain unit tests, no iced

Anything in `core` is pure data and logic, so it tests like any other Rust
crate: construct inputs, call the function, assert on the output. No widgets, no
simulator, no iced anywhere in the test — that's the whole payoff of keeping
iced out of `core`.

```rust
#[test]
fn a_full_house_beats_two_pair() {
    let full_house = Hand::parse("KKKQQ").unwrap();
    let two_pair = Hand::parse("KKQQ7").unwrap();
    assert!(full_house.rank() > two_pair.rank());
}
```

If a "core" test finds itself reaching for an iced type, the logic is in the
wrong crate — push it down into `core`, or test it through the GUI instead.

### iced screens — drive them with `iced_test::simulator`

A screen cell (`State` / `update` / `view`) *is* iced code: its `view` builds a
real element tree, so you can't exercise it without a renderer. That's what
`iced_test::simulator` is for — it lays out the interface headlessly, finds
widgets by their label text, lets you click / type / press keys, and hands back
the messages those interactions produced. Feed them through `update` and assert
on the resulting state (and, by re-simulating, on what's drawn).

`iced_test` is its own crate, added as a dev-dependency. It does **not** require
the `debug` (or `tester`) feature on `iced` — those are separate runtime devtools:

```toml
[dev-dependencies]
iced_test = "0.15.0-dev"   # or { workspace = true } in a workspace
```

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use iced_test::{Error, simulator};

    #[test]
    fn clicking_increment_twice_counts_to_two() -> Result<(), Error> {
        let mut counter = Counter { value: 0 };
        let mut ui = simulator(counter.view());

        let _ = ui.click("Increment")?;   // selectors match by label text
        let _ = ui.click("Increment")?;

        for message in ui.into_messages() {
            counter.update(message);
        }
        assert_eq!(counter.value, 2);

        // re-simulate the fresh view to assert on what's actually rendered
        let mut ui = simulator(counter.view());
        assert!(ui.find("2").is_ok(), "counter should display 2");
        Ok(())
    }
}
```

`.click()` / `.tap_key()` / `.typewrite()` drive interaction, `.find()` asserts
a widget is present, and `.into_messages()` drains the messages produced. Note
`.click()` returns a `Result` you must consume — bind it with `let _ =` when you
don't need the hit target.

### Test names read like sentences

```rust
#[test]
fn submitting_an_empty_form_keeps_state_idle() { ... }

#[test]
fn auth_result_ok_emits_authenticated_instruction() { ... }
```

No `test_` prefix — the `#[test]` attribute is sufficient.

---

## 6. Common pitfalls

### State holds an `Element` or widget handle

```rust
// BAD
pub struct State {
    cached_button: Element<'static, Message>,  // ← can't work
}
```

iced rebuilds the element tree every render. Build elements inside `view`,
return them, drop them. State holds DATA; view builds the tree from data.

### `Element<'static>` chasing

When you find yourself reaching for `Box::leak` or `'static` lifetimes on
Elements, you've taken a wrong turn somewhere. The right answer is usually:

- Pass the state as `&self` (or `&AppState`) into the function, return
  `Element<'_, Message>` with the elided lifetime — it'll tie to the borrow.

### Subscriptions that mutate state inside the closure

Subscription closures are pure — `event → Option<Message>`. Don't capture
`&mut self` or try to mutate state in the closure. Emit a Message and handle
it in `update`.

### Reaching for `Arc<Mutex<...>>`

Almost always a sign you're fighting TEA instead of working with it. The state
*is* shared inside the runtime; you don't need to share it with channels or
mutexes within an app. (External async work that holds the state across
`.await` points is a different case — that's `Task::perform` territory.)

### One big `Message` enum across the whole app

A 50-variant top-level `Message` with `LoginEmailChanged(String)` and
`DashboardWidgetClicked(WidgetId)` and `SettingsThemeChanged(Theme)` is the
"haven't started splitting yet" smell. Each screen gets its own Message; the
parent enum has one variant per screen (`Auth(auth::Message)`, `Dashboard(...)`).

### Trying to share child state across siblings via the parent

If `auth::State` and `dashboard::State` need to share a piece of data, that
data lives at the app level, not in a child. Pass it down as its own named
read-only argument to the child's `update` (a `&Client`, a `&Settings` — not a
catch-all `&AppContext`), or have the child surface an Instruction that the
parent uses to mutate the shared state.

---

## 7. Quick reference

When starting a new iced app:

1. Decide workspace vs single crate (test-the-logic threshold).
2. Set up workspace: root bin (`src/`) + a `core` crate for pure data/logic.
   Short crate names. No `ui` crate unless the app is large and needs generic,
   app-agnostic widgets.
3. Define the top-level `App` struct + `Screen` enum + `Message` enum in `src/lib.rs`.
4. Write the first screen as a self-contained TEA cell: `State`, `Message`,
   `Instruction`, `update → Action<I, M>`, `view`, `subscription`.
5. Wire parent's `update`/`view`/`subscription` to dispatch + `.map()`.
6. Add `Action<I, M>` to `src/action.rs` (or wherever).
7. iced API: function helpers (`button(text("..."))` not `Button::new`),
   `keyboard::listen().filter_map(...)`, no aliased imports, module-path
   naming.
8. Tests: drive Messages directly against the logic; `iced_test::simulator`
   for view-layer tests.

When refactoring an iced codebase that doesn't compose:

1. Identify the top-level Message enum. If >10 variants and they mention
   specific screens, you have screen-soup. Refactor toward per-module Message.
2. Look for `unwrap()` in update/view — replace with `expect("invariant: ...")`
   or `let Some(...) else { return ... }`.
3. Look for `Widget::new(...)` calls — replace with function helpers.
4. Look for composite type names (`FooState`, `FooMessage`) — rename inside
   their module to `State`, `Message`, use module-path disambiguation.
5. Look for `Arc<Mutex<...>>` in app state — usually a TEA-fight; move the
   shared data to app-level state, pass `&` references down.
6. Look for `Box::leak` or `'static` chasing — almost certainly wrong; fix
   by threading `&self`/`&AppState` through the call.

This skill pairs naturally with `guides/RUST_STYLE.md`. When in
doubt about a Rust-level question (error handling, dependencies, naming),
defer to that guide. Iced-specific questions live here.
