---
name: iced
description: Compose and structure iced (iced-rs) GUI applications in Rust. Workspace layout, TEA composition with Action<I, M> + .map() bubbling, iced API conventions (function helpers over Widget::new, impl Widget views and .boxed(), keyboard listen/filter_map, widget composition, custom widgets), iced_test simulator tests, and style. Use when starting an iced project, adding a screen, refactoring an iced state machine, or reviewing iced code that doesn't compose cleanly.
---

# /iced

Conventions for writing iced GUI code that composes. Four parts: **workspace**,
**TEA composition**, **iced API**, **Rust style**. Apply together — each
reinforces the others.

The canonical reference for raw Rust style is `guides/RUST_STYLE.md`. The
`guides/` folder sits beside the `skills/` folder this skill was installed
into (`~/.claude/guides`, `~/.codex/guides`, or your own agent's home).
This skill is the iced-specific addendum + idioms.

**Which iced.** Everything API-specific here is checked against iced
`0.15.0-dev` at git rev `cd04b74` (master on 2026-10-21; the CHANGELOG stops
at 0.14.0, so the git log is the only record of what changed since). iced
master moves fast and breaks freely: when the project pins a different rev,
or something here doesn't compile, read the source of the pinned rev —
cargo checks it out under `~/.cargo/git/checkouts/iced-*/<short rev>/` — and
trust it over this file and over memory. Most of what older iced code (and
most training data) does with `Element`, `.into()` and `Widget::layout` no
longer compiles at this rev; §3 says what replaced it, and §6 has a
before/after table.

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

[dev-dependencies]
iced_test = { workspace = true }

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
# `tokio`: the executor, and `iced::time::every`. `sipper`: `Task::sip`.
# `advanced`: custom widgets. Widgets like `image`, `svg`, `canvas` and
# `markdown` are features too.
iced = { version = "0.15.0-dev", features = ["tokio", "sipper", "image"] }
iced_test = "0.15.0-dev"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
thiserror = "2"

# Replace the crates.io iced with a commit of its master branch. Pin a `rev`,
# not `branch = "master"`: master breaks APIs between commits, and a branch
# moves on the next `cargo update`. Patch every iced crate that lands in your
# tree from the SAME source — the umbrella and `iced_test` here; add
# iced_core / iced_widget if a member depends on them directly.
[patch.crates-io]
iced = { git = "https://github.com/iced-rs/iced", rev = "<full commit hash>" }
iced_test = { git = "https://github.com/iced-rs/iced", rev = "<full commit hash>" }
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

use iced::widget::column;
use iced::{Subscription, Task, Widget, keyboard};
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

    pub fn view(&self) -> impl Widget<Message> {
        // a widget typed against THIS screen's Message, not the parent's
        column![...]
    }

    pub fn subscription(&self) -> Subscription<Message> {
        keyboard::listen().filter_map(...)
    }
}
```

The screen module exports its **own** `Message`, `Instruction`, `State`,
`update`, `view`, `subscription`. The parent never sees the screen's internal
Message variants directly.

`view` returns `impl Widget<Message>`: at this rev any widget can be a view, and
`Element` is just the boxed one (see "Widgets and `Element`" in §3). In edition
2024 the opaque type captures every lifetime in scope on its own, so
`-> impl Widget<Message>` is enough even when it borrows from several
arguments. The one case to spell out: a helper taking a reference it doesn't
keep (a `&str` it only formats) — narrow the captures with
`-> impl Widget<Message> + use<'a, Message>`, or callers can't pass a
temporary like `&format!(…)`.

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

use iced::{Element, Subscription, Task, Widget};
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
        // The arms are different widget types: box each into an `Element`.
        match &self.screen {
            Screen::Auth(s) => s.view().map(Message::Auth).boxed(),
            Screen::Dashboard(s) => s.view().map(Message::Dashboard).boxed(),
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
- `child.view().map(Message::Variant)` — for views (`.boxed()` where branches
  differ in type; `.map` and `.boxed` are methods of the `iced::Widget` trait,
  so it must be imported)
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

Subscriptions are the exception: `Subscription::map` and `filter_map` take only
**non-capturing** closures (the closure's type is part of the subscription's
identity), so neither `.with(id)` nor `move |m| … id …` works there. The check
is a `const` assertion evaluated at monomorphization: `cargo check` and
rust-analyzer pass it, and only `cargo build` / `cargo test` fail, with "The
Subscription closure provided is not non-capturing". Attach the id with
`Subscription::with`, which also makes it part of the identity:

```rust
child.subscription().with(id).map(|(id, message)| Message::Todo { id, message })
```

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
| Iced widget operations (focus, scroll) | no | yes (`operation::focus_next()` etc. are already `Task`s) |
| Both at once | `Action::new(i, t)` or `instruction(i).with_task(t)` | n/a |

If a child screen needs to navigate but ALSO kick off a fetch, it's two effects
in one Action — return `Action::new(instruction, task)`. The parent matches the
instruction, then returns the task.

### Subscription composition

Subscriptions follow the same `.map()` pattern. Multiple subscriptions per
screen combine with `Subscription::batch`:

```rust
use iced::time::{self, milliseconds};

pub fn subscription(&self) -> Subscription<Message> {
    Subscription::batch([
        keyboard::listen().filter_map(handle_key),
        time::every(milliseconds(100)).map(|_| Message::Tick),
    ])
}
```

Parent composes per-screen subscriptions the same way as views/tasks.
`time::every` exists only with the `tokio` or `smol` feature (the default
thread-pool executor has no timer). A subscription that should only run in
some state is just absent otherwise: return `Subscription::none()` in that
arm. Long-lived workers are `Subscription::run(fn)` / `run_with(data, fn)`,
which take function pointers, not closures, for the same identity reason.

### Inner function signatures are free-form

**Only the top-level boot, `update` and `view` functions** (the ones passed to
`iced::application(boot, update, view).run()`) have fixed signatures — and even
those are loose: `update` is `Fn(&mut State, Message) -> impl Into<Task<Message>>`
(so returning `()` works), `view` is `Fn(&State) -> impl Widget<Message>`. Inner
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

### Widgets and `Element`

At this rev widgets are generic over their content (`Button<'a, Message, W>`,
`Container<'a, W>`, `Column<W>`), and a view is any `impl Widget<Message>`.
`Element<'a, Message>` is the type-erased one — a `Box<dyn Widget>` — and the
**only** way to make one is `.boxed()`:

- **There is no `From<widget> for Element`.** `.into()` to an `Element` does not
  compile any more; neither does `Element::new` (it's private). Return
  `impl Widget<Message>`, or call `.boxed()`.
- **Box where types differ**: `match`/`if` arms, a `Vec` or iterator of mixed
  widgets, a struct field. Elsewhere leave widgets unboxed.
- **`use iced::Widget;`** wherever you call `.map(…)`, `.boxed()` or
  `.explain(color)` (draws the layout's bounds; handy for debugging) — they're
  methods of that trait. In custom-widget code that imports
  `iced::advanced::Widget` (the core trait, same name), call
  `iced::Widget::boxed(content)` in full.
- **`column![…]`, `row![…]`, `stack![…]`, `grid![…]` box each child**, so mixed
  children are fine. The function forms (`column(iter)`, `row(iter)`,
  `keyed_column(iter)`) take one child type `W`: box the items when they differ.
- **`Option<W>` is a widget, and `None` disappears**: containers drop it (no
  spacing is left for it), as they do `void()`. Write
  `column![header, maybe_error, body]` with `maybe_error: Option<_>` instead of
  building the column conditionally — but mind that the children after it
  change position when it comes and goes, which costs a stateful one (a
  scrollable, a text input) its state; see the gotchas below. Inherent
  `Option::map` still wins over `Widget::map`, so `maybe.map(f)` maps the
  option, not its messages.
- `&str` is a widget too: `column!["Top", row!["Left", "Right"]]`,
  `button("Save")`.

```rust
fn view(&self) -> impl Widget<Message> {
    let status = match &self.error {
        Some(error) => text(error).style(text::danger).boxed(),
        None => row![text("Ready"), space::horizontal()].boxed(),
    };

    column![
        text(&self.title).size(24),
        self.notice.as_ref().map(|notice| text(notice).size(13)),  // Option: shown or gone
        status,
    ]
    .spacing(8)
}
```

### Function helpers over `::new()`

iced's widgets all have function-helper constructors. **Use them.** The
`Widget::new(...)` form is legacy / internal-feeling.

```rust
// Bad
let s = Space::new().width(Length::Fill);
let b = Button::new(Text::new("submit"));
let c = Container::new(col).padding(20);

// Good
let s = space::horizontal();          // Length::Fill width
let b = button("submit");             // or button(text("submit").size(14))
let c = container(col).padding(20);
```

Catalog (at the pinned 0.15-dev; all in `iced::widget`):

| Helper | Replaces | Notes |
|---|---|---|
| `text(x)` / `text!("{n} left")` | `Text::new(…)` | takes `&str`, `String`, `&String`, numbers, `char`, `bool`; `value(x)` for any `ToString` |
| `rich_text![span("a"), span("b").color(c)]` | `text::Rich::…` | `.on_link_click(Message::Link)` (or `never` when there are no links) |
| `button(content)` | `Button::new(content)` | `.on_press(m)`, `.on_press_maybe(Option<m>)`, `.on_press_with(\|\| m)`; none = disabled |
| `container(content)` | `Container::new(content)` | `.padding`, `.width`, `.center(Fill)`, `.align_x`, `.id`, `.style` |
| `column![a, b, c]` macro | `Column::new().push(a).push(b)...` | `column(iter)` for dynamic; `.spacing`, `.align_x`, `.wrap()` |
| `row![a, b, c]` macro | `Row::new()...` | same; `.align_y` |
| `space()` / `space::horizontal()` / `space::vertical()` | `Space::new()` | empty, `Fill` wide, `Fill` tall; `.width()` / `.height()` |
| `text_input(placeholder, value)` | `TextInput::new(...)` | `.on_input(Message::Changed)` (none = disabled), `.on_submit`, `.id` |
| `checkbox(value)` | `Checkbox::new(...)` | `.label("…").on_toggle(Message::Toggled)` |
| `toggler(value)` | `Toggler::new(...)` | same shape as `checkbox` |
| `slider(range, value, on_change)` | `Slider::new(...)` | |
| `pick_list(selected, options, to_string)` | `PickList::new(...)` | `.on_select(Message::Picked)`; see "Pick lists" |
| `scrollable(content)` | `Scrollable::new(...)` | `.id`, `.on_scroll`, `.anchor_bottom()`, `.spacing` |
| `stack![a, b, c]` macro | `Stack::new()...` | layers, back to front |
| `tooltip(content, tip, tooltip::Position::Bottom)` | `Tooltip::new(...)` | `.delay(…)`, `.gap(…)` |
| `mouse_area(content)` | `MouseArea::new(...)` | `.on_press`, `.on_enter`, `.on_exit`, `.on_scroll`, `.interaction` |
| `float(content)` | `Float::new(...)` | draws its content lifted (`.scale`, `.translate`) over the rest |
| `image(handle)` / `svg(handle)` | `Image::new` / `Svg::new` | `image` / `svg` features |

Also there: `opaque` (swallows clicks that would pass through a `stack` layer),
`hover(base, top)`, `pin`, `sticky`, `sensor` (messages on show/hide/resize),
`responsive(|size| …)`, `lazy`, `keyed_column`, `grid`, `table`, `themer`,
`transition`, `void()`.

Plus alignment shorthands:

| Shorthand | Replaces |
|---|---|
| `center(content)` | `container(content).center(Fill)` |
| `center_x` / `center_y` / `right` / `bottom` / `bottom_right`… | the matching `container(…).align_*` |
| `iced::Center` (const) | `Alignment::Center` literal |
| `iced::{Left, Right, Top, Bottom}` | `alignment::Horizontal::Left` … |

### Keyboard subscriptions

Use `keyboard::listen().filter_map(...)`. **`on_key_press` does not exist**
in current iced. The listen-then-filter pattern is canonical; a named `fn`
keeps the subscription line short and is non-capturing by construction:

```rust
use iced::keyboard::{self, key::Named, Key};

fn shortcut(event: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
        return None;
    };

    match key.as_ref() {
        Key::Named(Named::Enter) => Some(Message::Submit),
        Key::Named(Named::Escape) => Some(Message::Cancel),
        Key::Character("f") if modifiers.command() => Some(Message::FocusSearch),
        _ => None,
    }
}

// in `subscription`
keyboard::listen().filter_map(shortcut)
```

`key.as_ref()` turns `Key<SmolStr>` into `Key<&str>`, so characters match as
string literals. `modifiers.command()` is Ctrl, or Cmd on macOS. For arrow keys,
function keys, tabs — `Key::Named(Named::ArrowLeft)`, etc.

`keyboard::listen()` only sees keys **no widget captured**: while a
`text_input` has the focus, what it types, copies or pastes goes to the input
and not to the subscription (keys it doesn't act on still come through). For
every event, captured or not, use `event::listen_with(f)`, where `f` is a plain
`fn(Event, event::Status, window::Id) -> Option<Message>` (window focus,
file drops and the like come through it too).

### Layout helpers

| Type | Meaning |
|---|---|
| `Length::Fit` | the default for most widgets: the content's size — but a `Fill` child makes it fill (containers inherit their children's lengths) |
| `Length::Fill` | take all available space along this axis |
| `Length::FillPortion(n)` | take `n/total` of available space, where total is the sum of FillPortions on this axis |
| `Length::Shrink` | the least space: the content's size, compressing `Fill` children to theirs |
| `Length::Fixed(px)` | exactly N logical pixels; `.width(300)` / `.width(12.5)` convert from `u32` / `f32` |
| `Fill.max(600)`, `Fit.max(360)`, `Fill.min(200)` | a `Length::Bounded`: the strategy, clamped. This is what `max_width` used to be (containers have no `max_width` now) |
| `iced::{Fill, FillPortion, Fit, Shrink}` | the variants, re-exported at the root |

`Fit.max(360)` is the idiom for a tooltip or a dialog that wraps its text past
a width but stays small when the text is short.

Padding: `.padding(10)`, `.padding([4, 8])` (vertical, horizontal), or the
`iced::padding` module for one side: `padding::top(6)`, `padding::all(8).left(0)`.
There is no `padding!` macro.

### Theme + style closures

```rust
use iced::{Color, Theme, border, color};

container(content)
    .style(|_theme: &Theme| container::Style {
        background: Some(MY_COLOR.into()),
        text_color: Some(Color::WHITE),
        border: border::rounded(4).color(GOLD).width(1),
        ..container::Style::default()
    })
```

Style closures take `&Theme` (plus a `Status` for interactive widgets:
`|theme: &Theme, status: button::Status|`) and return the widget's `Style`.
They run every time the widget is drawn — keep them cheap; precompute colors as
`const`. Each widget module ships ready-made ones to start from or fall back
to: `button::primary` / `secondary` / `danger` / `text` / `subtle`,
`container::rounded_box` / `bordered_box`, `text::secondary` / `danger`.

Style structs gained fields over time; end struct literals with
`..X::Style::default()` so they keep compiling. Some of them: `button::Style`,
`container::Style` have `snap: bool` (round the quad to whole pixels; the
default follows the `crisp` feature, on by default), `text::Style` has
`selection`, `scrollable::Style` has `auto_scroll`.

A theme of your own is `Theme::custom("Name", theme::palette::Seed { background,
text, primary, success, warning, danger })`; iced derives the rest. Styles read
it back with `theme.palette()` (swatches with `base` / `weak` / `strong` pairs:
`palette.primary.base.color`, `palette.background.strong.color`,
`palette.danger.base.text`) or `theme.seed()` for the six seed colors.

### Text

```rust
text(&entry.path)
    .size(14)
    .wrapping(text::Wrapping::WordOrGlyph)  // break a long path anywhere when a word won't fit
    .selectable(true)                       // the user can select and copy it

text(&entry.name)                                // "Some very long na…"
    .wrapping(text::Wrapping::None)
    .ellipsis(text::Ellipsis::End)
```

- `text::Wrapping`: `Word` (default), `Glyph`, `WordOrGlyph`, `None`.
  `text::Ellipsis`: `None` (default), `Start`, `Middle`, `End`. It shortens the
  last line that fits the text's bounds, so it shows on a single line
  (`Wrapping::None`, the way a `pick_list` lays out its label) or under a
  bounded height; wrapping text with room to grow just wraps. `pick_list` has
  `.ellipsis(…)` too.
- **Selectable text** — `text(…).selectable(true)` and
  `rich_text(…).selectable(true)` — is driven by the runtime
  (`UserInterface`), not the widget: a left press that **no widget captured**
  starts a selection, dragging extends it (a double click selects words, a
  triple click lines), across every selectable text it crosses, and Ctrl+C
  (Cmd+C on macOS) copies it — again only if no widget captured the key. So
  text inside a `button` or a `mouse_area` with `on_press` is never selectable
  (they capture the press), and a focused `text_input` keeps Ctrl+C for itself.
  Nothing to wire in `update`.
- `text` has no `Renderer` parameter: the type is `iced::widget::Text<'a>` (or
  `Text<'a, Theme>`), handy for helpers that return a styled text.
- Sizes are logical pixels; the default size is 16 and the default line height
  `LineHeight::Relative(1.375)`, both set app-wide with
  `iced::Settings { text_size, line_height, .. }` or `.line_height(…)` on the
  application builder.

### Pick lists

```rust
pick_list(self.locale, Locale::ALL, Locale::to_string)  // selected: Option<V>, options, label
    .on_select(Message::LocaleChanged)                    // without it: drawn disabled, never opens
    .placeholder("Language")
```

- `pick_list(selected, options, to_string)`: `selected` is `Option<V>` where
  `V: Borrow<T>` (so `Option<T>` or `Option<&T>`), `options` anything
  `Borrow<[T]>` (an array, a `Vec`, a slice), and the label is a closure
  `Fn(&T) -> String` — the options needn't implement `Display`.
- `on_select` is optional: leave it out and the list is disabled (its style
  gets `pick_list::Status::Disabled`), which is how to show a choice that can't
  be changed right now.
- The opened menu is exactly as wide as the closed box. With the default
  `Length::Shrink` width the box sizes itself to the widest option; give it a
  width (`Fill`, a fixed one) and long options are cut, so pair it with
  `.ellipsis(text::Ellipsis::End)`.
- The menu is styled apart from the box: `.style(…)` for the box,
  `.menu_style(|theme| menu::Style { … })` (`iced::widget::overlay::menu`) for
  the menu.

### `Task` building blocks

```rust
Task::none()                              // no-op
Task::done(message)                       // synchronous result
Task::perform(future, Message::Loaded)    // async; the closure maps the future's output
Task::run(stream, Message::Line)          // every item of a stream
Task::sip(sipper, Message::Progress, Message::Done)  // progress, then a final output (`sipper` feature)
Task::batch([a, b, c])                    // run in parallel
a.chain(b)                                // run sequentially
a.then(|output| next_task(output))        // a new task per output
task.map(Message::Inner)                  // pass the variant itself — not |message| Message::Inner(message)
task.discard()                            // keep the effect, drop the output
```

For widget-level operations (focus a field, scroll a container) use the
`iced::widget::operation` module. **Its functions already return a `Task`** —
don't wrap them in `Task::done`:

```rust
use iced::widget::operation::{self, Animation};
use iced::widget::operation::scrollable::{AbsoluteOffset, RelativeOffset};

operation::focus("search")                      // any `impl Into<widget::Id>`; `&'static str` works
operation::focus_next()
operation::scrollable::snap_to("list", RelativeOffset::START, Animation::Instant)
operation::scrollable::scroll_to("list", AbsoluteOffset { x: None, y: Some(y) }, Animation::Smooth)
```

The target widget carries the same id: `text_input(…).id("search")`,
`scrollable(…).id("list")`, `container(…).id(…)`. A custom
`widget::Operation` (see "Custom widgets") runs as a task with
`iced::advanced::widget::operate(op)`, and its output can drive the next task:
`operate(locate).then(|found| …)`.

**Cancelling.** `let (task, handle) = task.abortable();` — keep
`handle.abort_on_drop()` in the state the work belongs to, and dropping that
state (leaving the screen, replacing the job) aborts the task.

**Progress with `Task::sip`.** A *sipper* is a future that also streams
progress. Build one with `iced::task::sipper` (`Straw<Output, Progress, Error>`
is a sipper whose output is a `Result`); the `Sender` type comes from the
`sipper` crate itself, so depend on it (`sipper = "0.2"`) when a helper takes one:

```rust
use iced::task::{Straw, sipper};

fn download(url: String) -> impl Straw<Vec<u8>, Progress, Error> {
    sipper(async move |mut progress| {
        // …
        progress.send(Progress { received, total }).await;
        // …
        Ok(bytes)
    })
}

// in `update`
Task::sip(download(url), Message::Progressed.with(id), Message::Finished.with(id))
```

`Task::units()` counts the work a task holds (`Task::none()` and `Task::done`
hold none): enough for a test to assert that an `update` started something.

### Color construction

**Reach for `iced::color!(0xRRGGBB)`.** It's the first-class way to write a
color and the right choice 99% of the time — everything else is a hack by
comparison.

```rust
let accent = color!(0xd4a64a);
let translucent = color!(0xd4a64a, 0.5);   // optional alpha
```

It works in a `const` too — it expands to `Color::from_packed_rgba8`, a
`const fn`, behind a const-safe range check — so a palette of named colors is
just:

```rust
pub const GOLD: Color = color!(0xffc700);
pub const SHADOW: Color = color!(0x000000, 0.7);
```

`Color`'s own constructors (`from_rgb8`, `from_rgba8`, `from_packed_rgb8`,
`from_packed_rgba8`, the `f32` `from_rgb` / `from_rgba`) are all `const` as
well, but there's no reason to reach for them over `color!`. For a variant of a
color, `Color { a: 0.5, ..GOLD }`, `GOLD.scale_alpha(0.5)` or
`GOLD.mix(other, 0.3)`.

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
pub fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title(App::title)            // or a `&'static str`
        .theme(App::theme)            // `Fn(&State) -> impl Into<Option<Theme>>`, or a `Theme`
        .subscription(App::subscription)
        .fonts(fonts())               // byte slices loaded before the first frame
        .window(window::Settings { min_size: Some(MIN), ..window::Settings::default() })
        .centered()
        .run()
}
```

iced's `BootFn` impls cover both `() -> Self` and `() -> (Self, Task<Message>)`,
so either works. Use the tuple form whenever the app needs to kick off async
work at startup. `iced::run(update, view)` is the shortcut for a `Default`
state and no builder options. A theme function returning `None` follows the
system's light/dark mode.

### Stack widget for layering

```rust
use iced::widget::{center, container, opaque, stack};

stack![
    content,                                         // back layer
    dialog.map(|dialog| opaque(center(dialog))),     // Option: a layer only while there's a dialog
]
```

Use `stack!` when you want one widget visually on top of another — a modal over
a form, a toast over the content, a floating toolbar over a scrollable. An
`Option` layer that is `None` simply isn't there, and `content` keeps its place
(and so its widget state: scroll offsets, focus) whether the dialog shows or
not. Events go to the top layer first, and whatever it doesn't capture reaches
the layers below — a click on the empty part of a full-window layer lands on
the content under it. Wrap a layer in `opaque` (it captures presses over its
bounds; a `mouse_area` with `on_press` does too) to stop that. A layer that
should only show on hover is `hover(base, top)`.

### Custom widgets (the `advanced` feature)

Reach for one only when composing built-in widgets can't do it: a drawing of
your own, an event behavior no widget has. The trait is
`iced::advanced::Widget` (the core trait, no defaults for `Theme`/`Renderer`);
`iced::Widget` is the extension trait with `.map`/`.boxed`, implemented for
every core widget. Abridged from `core/src/widget.rs` at this rev:

```rust
pub trait Widget<Message, Theme, Renderer>: Meta {
    fn size(&self) -> Size<Length>;
    fn tag(&self) -> tree::Tag { tree::Tag::stateless() }      // `Tag::of::<State>()`
    fn state(&self) -> tree::State { tree::State::None }       // `tree::State::new(State::default())`
    fn diff(&mut self, tree: &mut Tree) { tree.children.clear(); }
    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits);
    fn draw(&self, tree: &Tree, renderer: &mut Renderer, theme: &Theme,
            style: &renderer::Style, layout: Layout, cursor: mouse::Cursor, viewport: &Rectangle);
    fn operate(&mut self, tree: &mut Tree, layout: Layout, viewport: &Rectangle,
               renderer: &Renderer, operation: &mut dyn Operation) {}
    fn update(&mut self, tree: &mut Tree, event: &Event, layout: Layout, cursor: mouse::Cursor,
              renderer: &Renderer, shell: &mut Shell<'_, Message>, viewport: &Rectangle) {}
    fn mouse_interaction(&self, tree: &Tree, layout: Layout, cursor: mouse::Cursor,
                         viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction { … }
    fn overlay<'a>(&'a mut self, tree: &'a mut Tree, layout: Layout, renderer: &Renderer,
                   viewport: &Rectangle, translation: Vector, window: Size)
        -> Vec<overlay::Element<'a, Message, Theme, Renderer>> { Vec::new() }
}
```

What differs from the iced most examples online show:

- **`widget::Meta` is a supertrait.** Add `impl Meta for MyWidget {}` (its one
  method, `is_void`, defaults to `false`); `column`/`row` children need it too.
- **Layout lives in the tree.** `layout` returns nothing: it stores the size in
  `tree.size`, and lays each child out into `tree.children[i]` (which then holds
  the child's size) and sets `tree.children[i].translation`, the child's offset
  in the parent. There is no `layout::Node` and no `Widget::children()`: a
  widget with children creates their trees in `diff`
  (`tree.diff_children(&mut self.children)`); the default `diff` clears them.
- **Helpers fill the tree in**: `tree.size = layout::atomic(limits, w, h)` for a
  leaf; `layout::padded(tree, limits, w, h, padding, |child, limits| { content.layout(child, renderer, limits); child.size })`
  (and `contained`, `positioned`) for one child.
- **`Layout` is only a position and a size.** `layout.bounds()`;
  `layout.iter(&tree.children)` / `layout.iter_mut(&mut tree.children)` give
  `(child_layout, child_tree)` pairs to draw, update and operate children with;
  `Layout::new(size).move_to(point)` makes one by hand.
- **Lengths inherit in `diff`**: `self.width = self.width.stack(child.size().width)`
  is how a `Fit` button around a `Fill` child comes to fill.
- **`update` returns nothing** — talk through the `Shell`: `shell.publish(m)`,
  `shell.capture_event()` (marks the event handled: lower `stack` layers, the
  `keyboard::listen` subscription and text selection no longer get it; a
  `column` still hands it to its other children, which may check
  `shell.is_event_captured()`), `shell.request_redraw()` (animations: the
  next frame arrives as `Event::Window(window::Event::RedrawRequested(now))`),
  `shell.invalidate_layout()`.
- **`overlay` returns a `Vec`** (forward the children's with
  `overlay::from_children(…)`) and is told the `window` size. The `Overlay`
  trait has no `layout` method: an overlay lays its content out when it's built
  (diff the content's tree, lay it out against limits derived from `window`,
  `Layout::new(tree.size).move_to(position + translation)`) and keeps that
  `Layout`; its `draw` wraps itself in `renderer.with_layer(bounds, |r| …)`.
- **To wrap a widget**, store an `Element<'a, Message, Theme, Renderer>` built
  with `iced::Widget::boxed(content)`, forward `tag`, `state`, `diff`, `size`,
  `layout`, `draw`, `operate`, `mouse_interaction` and `overlay` to it, and
  change only the one behavior you need — e.g. an `update` that forwards, then
  captures the wheel as well so a list under a dialog doesn't scroll.

A leaf widget, whole:

```rust
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{Meta, Tree};
use iced::advanced::{Shell, Widget};
use iced::{Color, Event, Length, Rectangle, Size, border, mouse};

pub fn swatch<Message>(color: Color, on_press: Message) -> Swatch<Message> {
    Swatch { color, on_press }
}

pub struct Swatch<Message> {
    color: Color,
    on_press: Message,
}

impl<Message> Meta for Swatch<Message> {}

impl<Message: Clone, Theme, Renderer> Widget<Message, Theme, Renderer> for Swatch<Message>
where
    Renderer: renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(24.0), Length::Fixed(24.0))
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) {
        tree.size = layout::atomic(limits, 24.0, 24.0);
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let quad = Quad { bounds: layout.bounds(), border: border::rounded(4), ..Quad::default() };
        renderer.fill_quad(quad, self.color);
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
            && cursor.is_over(layout.bounds())
        {
            shell.publish(self.on_press.clone());
            shell.capture_event();
        }
    }
}
```

It goes straight into `row![swatch(GOLD, Message::Picked(GOLD)), …]` — no
`.into()`, no `Element::new`.

**Custom operations.** A `widget::Operation<T>` walks the widget tree:
`traverse` is the one required method (call `operate(self)` to visit the
children), and the callbacks it may override — `container`, `scrollable`,
`focusable`, `text_input`, `text`, `custom` — each receive the widget's
`Option<&Id>` and bounds; `finish` returns an `Outcome<T>`. Run it as a task
with `iced::advanced::widget::operate(op)` (a `Task<T>`), or on a test's
simulator with `ui.operate(&mut op)`:

```rust
use iced::advanced::widget::{Id, Operation, operation};
use iced::{Rectangle, Size, Vector};

/// How far down each scrollable is, in layout order.
struct Offsets(Vec<f32>);

impl Operation for Offsets {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn scrollable(&mut self, _id: Option<&Id>, _bounds: Rectangle, _content: Size,
                  translation: Vector, _state: &mut dyn operation::Scrollable) {
        self.0.push(translation.y);
    }
}
```

Scrollables report the translation they were last drawn with, so in a test
call `ui.draw(&theme)` before operating.

### Common iced 0.15-dev gotchas

- Feature-gated: `image` (or `image-without-codecs`), `svg`, `canvas`,
  `qr_code`, `markdown`, `highlighter`, `selector`; `advanced` for custom
  widgets and the `.class(…)` builders; `sipper` for `Task::sip`; `tokio` or
  `smol` for `time::every`. Add the feature to your iced dependency or the
  imports fail.
- Builder methods take `impl Into<Length>` / `impl Into<Pixels>` and accept bare
  integers through the `From<u32>` impls — `.height(8)` is
  `Length::Fixed(8.0)`.
- `Element<'a, Message>` lifetime is tied to whatever widgets borrow from in
  the `view` body. Don't try to construct an `Element<'static>` and store it
  in `State` — iced rebuilds the element tree every render. Build elements
  inside `view`; let lifetimes flow naturally.
- Widget state (scroll offset, focus, a text input's cursor) lives in iced's
  widget tree, matched to the new view by **position** each frame (and by
  widget type). A widget that moves to another place in the tree — wrapped in
  a container on one frame and not the next, or pushed down by a sibling that
  appeared before it (an `Option` child going from `None` to `Some` shifts every
  later sibling) — loses its state or takes another's. Keep stateful widgets at
  a fixed position: put the children that come and go in an inner column of
  their own, `column![column![header, banner, notice], scrollable(body)]`, so
  the scrollable stays the second child whatever the banners do.
  `keyed_column` matches children by key, but by a heuristic that gets some
  changes wrong (an item inserted just before the last one is taken for an
  append), so don't rely on it for state that matters.
- `Text` widgets' size is in logical pixels. Default size is 16 and default
  line height 1.375; size-20 text is noticeably larger.

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
`view` is different. Composing a view out of separate `-> impl Widget<Message>`
fragments is genuinely helpful *and* the iced idiom — a widget is a value,
and building it from smaller value-returning functions reads better than one
monolithic `view`, even when a fragment is used once. So for views, treat
extraction as a ladder you climb only as far as the app actually needs:

- **Level 0 — everything inline in one `fn view`.** The right default for a
  small screen. No fragments.
- **Level 1 — extract a semantically meaningful chunk as a function.** When part
  of a screen is itself composed of smaller pieces and reads as *a thing* — a
  card, a toolbar, a summary panel — give it a `fn card(...) -> impl
  Widget<Message>`. The win is comprehension: a `view` that reads `column![header(…),
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
  `impl Widget<Message> + 'a` you can drop into many screens — e.g.
  `fn badge<'a, Message: 'a>(label: &'a str, tone: Tone) -> impl Widget<Message> + 'a`.
  Take children as `impl Widget<Message> + 'a` too, so callers needn't box.
- **Level 3 — components as their own `Newtype<'a>` with a builder API.** When a
  component grows real structure, give it a type: `struct Card<'a, Message>`
  holding its data and children, with builder methods (`.footer(...)`, a rounded
  border applied to every child, a typed `submit: Option<Button<'a, Message>>`
  plus a `&str` label rather than a type-erased `Element`). Now it has an API,
  not just a signature. There's no `From<Card> for Element` to hang `.into()` on
  any more: end the builder in a method returning `impl Widget<Message>`, or,
  to drop the type itself into `column![]`, implement `Widget` for it by
  forwarding to the widget it builds (see "Custom widgets" in §3).
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
use iced::Widget;
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
    pub fn view(&self) -> impl Widget<Message> { ... }
}

// 6. Private helpers
fn build_input<'a>(label: &'a str, value: &'a str) -> impl Widget<Message> + 'a { ... }

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

Patch it from the same git rev as `iced` (see §1): from any other source or
rev it brings a second copy of iced's core, and your widgets' types no longer
match the simulator's.

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

The `Simulator` at this rev, in full:

- **Building.** `simulator(view)` lays the view out in a 1024×768 window with
  default `Settings` — so with iced's bundled Fira Sans, not the fonts your app
  loads. `Simulator::with_size(iced::Settings { fonts, ..Default::default() },
  (w, h), view)` when text measurement or a narrow window matters. A widget
  built inline in the test (rather than returned as `impl Widget<Message>`,
  whose trait defaults pin iced's `Theme` and `Renderer`) leaves those two
  uninferable — "type annotations needed"; annotate it:
  `let view: Element<'_, Message> = column![…].boxed();`.
- **Selectors** (what `find` and `click` take): a `&str` / `String` matches a
  text widget, or a text input's value, whose content **equals** it — the
  whole string, not a substring (a `pick_list` exposes its selected label or
  placeholder this way); `iced_test::selector::id("save")` (or a
  `widget::Id`) matches by id — give icon-only buttons an id
  (`container(icon).id("save")`) so tests can reach them;
  `selector::is_focused()`; a `Point`; or any closure
  `FnMut(selector::Candidate) -> Option<T>`.
- **Clicking.** `click(selector)` points the cursor at the center of the
  target's *visible* bounds and sends a press and a release; text inside a
  button clicks the button. A target scrolled or clipped out of view fails with
  `Error::TargetNotVisible`. A disabled button (no `on_press`) produces no
  message, which is how to assert it's off:
  `assert_eq!(ui.into_messages().count(), 0)`.
- **Pointing and raw events.** `point_at(point)` moves the cursor *without*
  sending `CursorMoved`; for hover effects (a tooltip) follow it with
  `ui.simulate([Event::Mouse(mouse::Event::CursorMoved { position })])`.
  `simulate(events)` sends any events and returns their statuses;
  `scroll(ScrollDelta::Lines { x: 0.0, y: -3.0 })` scrolls whatever is under
  the cursor (point at it first). Smooth scrolling glides over several frames:
  feed `Event::Window(window::Event::RedrawRequested(t))` events with
  increasing `t` before reading the result.
- **Typing.** Click the input first (`ui.click(selector::id("search"))`), then
  `ui.typewrite("text")` or `ui.tap_key(Key::Named(Named::Enter))`.
- **Messages.** `into_messages()` consumes the simulator; `drain()` takes the
  messages so far and keeps it going.
- **State across frames.** `ui.rebuild(new_view)` swaps in the view of the
  updated state, keeping widget state (scroll offsets, focus) the way the
  running app would — the way to test that something survives a re-render.
  `ui.resize(size)` relays the same view out.
- **Inspecting.** `ui.operate(&mut op)` runs a custom `widget::Operation` (see
  §3); `ui.draw(&theme)` draws (and processes a redraw first);
  `ui.snapshot(&theme)?.matches_hash("tests/snapshots/name")?` compares
  against (or, the first time, records) a hash of the rendered pixels.

Tests that need a whole running program — boot, tasks, subscriptions — have
`iced_test::Emulator` and the `.ice` file format, which iced itself flags as
very experimental; at screen level, the simulator plus calling `update`
directly covers it. `Task::units() > 0` asserts that an `update` started work
without running it.

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
  `impl Widget<Message>` (or `Element<'_, Message>`) — it'll tie to the borrow.

### Code written for an older iced

Most iced code in the wild — and most of what a model remembers — predates this
rev. The usual compile errors and what replaced them:

| Old | Now |
|---|---|
| `widget.into()` / `Element::from(widget)` / `Element::new(widget)` | `widget.boxed()` (with `use iced::Widget`), or return `impl Widget<Message>` |
| `fn layout(…) -> layout::Node` | `fn layout(&mut self, tree, renderer, limits)` writing `tree.size` and the children's trees |
| `fn children(&self) -> Vec<Tree>` | create them in `diff`: `tree.diff_children(&mut self.children)` |
| `fn diff(&self, …)`, `Layout<'a>`, `layout.children()` | `fn diff(&mut self, …)`, `Layout` (no lifetime), `layout.iter(&tree.children)` |
| `fn overlay(…) -> Option<overlay::Element>` | `-> Vec<overlay::Element>`, with `translation` and `window` arguments |
| `Space::new(w, h)`, `horizontal_space()` | `space().width(w)`, `space::horizontal()` |
| `container(…).max_width(600)` | `.width(Fill.max(600))` |
| `pick_list(options, selected, on_select)` | `pick_list(selected, options, to_string).on_select(on_select)` |
| `Task::done(operation::focus_next())` | `operation::focus_next()` (already a `Task`) |
| `palette::Extended`, `theme.extended_palette()` | `theme::palette::Palette`, `theme.palette()` |

### Subscriptions that mutate state inside the closure

Subscription closures are pure — `event → Option<Message>`. Don't capture
`&mut self` or try to mutate state in the closure. Emit a Message and handle
it in `update`. iced enforces the "don't capture" half: `Subscription::map` and
`filter_map` reject any closure that captures — at `cargo build`, not
`cargo check` (see "Routing" in §2 for `Subscription::with`).

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
   app-agnostic widgets. Pin iced (and `iced_test`) to one git `rev`.
3. Define the top-level `App` struct + `Screen` enum + `Message` enum in `src/lib.rs`.
4. Write the first screen as a self-contained TEA cell: `State`, `Message`,
   `Instruction`, `update → Action<I, M>`, `view → impl Widget<Message>`,
   `subscription`.
5. Wire parent's `update`/`view`/`subscription` to dispatch + `.map()`
   (`.boxed()` where `view`'s arms differ).
6. Add `Action<I, M>` to `src/action.rs` (or wherever).
7. iced API: function helpers (`button("…")` not `Button::new`), `.boxed()` not
   `.into()`, `keyboard::listen().filter_map(...)`, no aliased imports,
   module-path naming.
8. Tests: drive Messages directly against the logic; `iced_test::simulator`
   for view-layer tests.

When refactoring an iced codebase that doesn't compose:

1. Identify the top-level Message enum. If >10 variants and they mention
   specific screens, you have screen-soup. Refactor toward per-module Message.
2. Look for `unwrap()` in update/view — replace with `expect("invariant: ...")`
   or `let Some(...) else { return ... }`.
3. Look for `Widget::new(...)` calls — replace with function helpers. Look for
   code from an older iced (`.into()` to `Element`, `-> layout::Node`) — see
   "Code written for an older iced" in §6.
4. Look for composite type names (`FooState`, `FooMessage`) — rename inside
   their module to `State`, `Message`, use module-path disambiguation.
5. Look for `Arc<Mutex<...>>` in app state — usually a TEA-fight; move the
   shared data to app-level state, pass `&` references down.
6. Look for `Box::leak` or `'static` chasing — almost certainly wrong; fix
   by threading `&self`/`&AppState` through the call.

This skill pairs naturally with `guides/RUST_STYLE.md`. When in
doubt about a Rust-level question (error handling, dependencies, naming),
defer to that guide. Iced-specific questions live here.
