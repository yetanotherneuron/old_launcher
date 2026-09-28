# Push State Down

TEA composition at scale: when to use Instructions, when to push
`&mut` handles down, and how to keep `update` flat.

Companion to the `/iced` skill's TEA composition section. Read that
first for `Action<I, M>`, `.map()` composition, and basic screen
patterns.

---

## Starting point: the pocket guide pattern

The [iced pocket guide](https://docs.iced.rs) shows screen composition
via an `Action` enum the child returns from `update`:

```rust
// contacts.rs
pub enum Action {
    None,
    Run(Task<Message>),
    Chat(Contact),     // ← the parent navigates to a new screen
}
```

The parent matches this to drive transitions:

```rust
Message::Contacts(message) => {
    let action = contacts.update(message);
    match action {
        contacts::Action::Chat(contact) => {
            let (conversation, task) = Conversation::new(contact);
            state.screen = Screen::Conversation(conversation);
            task.map(Message::Conversation)
        }
        contacts::Action::Run(task) => task.map(Message::Contacts),
        contacts::Action::None => Task::none(),
    }
}
```

This is clean: `Chat` is **policy** — only the parent owns
`state.screen`. The child can't navigate; it signals the intent.

The question is what happens when a child signals ten things and
eight of them are mutations the parent performs mechanically on
state the child could have written directly.

---

## Policy vs plumbing

An Instruction exists because the child **cannot do the work itself**.
Before adding a variant, ask: can the child do this if I give it the
right `&mut` handle?

| Signal | Classification | Why |
|---|---|---|
| Navigate to a sibling screen | **Policy** | child doesn't own the screen enum |
| Provision a resource in a pool | **Policy** | child doesn't own the pool |
| Server sync / disk I/O | **Policy** | child has no I/O handle |
| Set a title on a shared struct | **Plumbing** | push `&mut title` down |
| Update a style on a companion | **Plumbing** | push `&mut companion` down |
| Refresh a preview after edits | **Plumbing** | push `&mut preview` down |

Plumbing Instructions are the parent doing the child's work for it.
They inflate the Instruction enum, add match arms in the parent, and
spawn single-use `route_*` helpers to host the translation.

---

## The smell: route_* helpers

A parent with a single-use method that matches a child's instructions:

```rust
fn route_editor(&mut self, message: editor::Message) -> Action<Instruction, Message> {
    let action = self.editor.update(message);
    let task = action.task.map(Message::Editor);
    match action.instruction {
        Some(editor::Instruction::TitleChanged(t)) => {
            self.title = t;           // plumbing
            Action::task(task)
        }
        Some(editor::Instruction::StyleUpdated(s)) => {
            self.style = s;           // plumbing
            self.refresh();
            Action::task(task)
        }
        Some(editor::Instruction::Save(data)) => {
            Action::task(task)        // policy — bubbles up
                .with_instruction(Instruction::Save(data))
        }
        None => Action::task(task),
    }
}
```

Two of three instructions are plumbing. `route_editor` exists because
the child wasn't given enough state.

**Fix**: push `&mut` handles down. The child mutates directly; only
`Save` remains:

```rust
// editor.rs
pub enum Instruction {
    Save(Data),   // the only policy variant
}

pub fn update(
    &mut self,
    message: Message,
    title: &mut String,
    style: &mut Style,
) -> Action<Instruction, Message> {
    match message {
        Message::SetTitle(t) => { *title = t; Action::none() }
        Message::EditStyle(s) => { *style = s; Action::none() }
        Message::Save => {
            Action::instruction(Instruction::Save(self.data.clone()))
        }
    }
}
```

The parent's match arm is flat — no helper needed:

```rust
Message::Editor(message) => {
    let action = self.editor.update(
        message, &mut self.title, &mut self.style,
    );
    if let Some(editor::Instruction::Save(data)) = action.instruction {
        // handle policy
    }
    action.task.map(Message::Editor)
}
```

---

## `fn update` is free-form

Only the top-level `update` (passed to `iced::application`) has a
fixed signature. Inner modules choose whatever shape fits:

```rust
// No effects — all mutations land through &mut params or &mut self
pub fn update(&mut self, message: Message);

// Async work, no policy signals
pub fn update(&mut self, message: Message) -> Task<Message>;

// Async work + policy signals
pub fn update(&mut self, message: Message) -> Action<Instruction, Message>;

// With immutable context as named params
pub fn update(
    &mut self,
    message: Message,
    catalog: &Catalog,
) -> Action<Instruction, Message>;

// With mutable handles to state the child writes directly
pub fn update(
    &mut self,
    message: Message,
    title: &mut String,
    companion: &mut Companion,
    provider: Option<&Provider>,
) -> Action<Instruction, Message>;

// Free function — when there's no meaningful Self
pub fn update(
    state: &mut State,
    message: Message,
    ctx: &Catalog,
) -> Action<Instruction, Message>;
```

Pick the narrowest return type:
- **`()`** — no async work, no policy signals
- **`Task<Message>`** — async work, parent never acts on outcome
- **`Action<Instruction, Message>`** — some messages need parent
  policy decisions

Pass context as **separate named params**. Never bundle into a god
`Context` struct — a grab-bag type hides what the child actually
needs and couples unrelated concerns.

---

## Blanket sweeps vs per-arm demand

A wrapper that diffs state before/after every message:

```rust
fn update(&mut self, message: Message) -> Task<Message> {
    let before = self.fingerprint();
    let task = self.update_inner(message);
    if self.fingerprint() != before {
        task.chain(self.refresh())
    } else {
        task
    }
}
```

Problems:
- Serialization on every message, including no-ops like `SelectTab`
- Hides which arms change state behind `update_inner`
- The wrapper/inner split is pure indirection

**Per-arm demand**: each arm knows whether it touched state.

```rust
fn update(&mut self, message: Message) -> Task<Message> {
    match message {
        Message::SetTitle(t) => {
            self.title = t;
            self.refresh()       // changed state — refresh
        }
        Message::SelectTab(tab) => {
            self.tab = tab;
            Task::none()         // didn't — skip
        }
    }
}
```

Flat. No wrapper. No wasted work.

---

## Default to inline — extraction needs justification

The default is **everything inline in the match arm**. A 20-line
arm is fine. A 40-line arm is fine. "It's getting long" is not a
reason to extract — length is the cost of being readable top-to-
bottom. Extract only when ALL of these hold:

1. **3+ call sites** — genuinely reused, not "I might call it again"
2. **Pure or nearly pure** — the helper doesn't reach into 5 fields
   of `&mut self`; if it does, it's just `update` wearing a mask
3. **Testable in isolation** — you'd actually write a unit test for
   the helper's logic, not just for the arm that calls it

A helper called from one arm is indirection, not abstraction. Inline
it. A helper that takes `&mut self` and touches the same fields as
`update` is `update` split into two methods for no reason. Inline it.

```rust
// Smell: "it was getting long so I extracted it"
impl State {
    pub fn update(&mut self, msg: Message) -> Action<...> {
        match msg {
            Message::Ai(m) => self.route_ai(m),     // hides 30 lines
            Message::Editor(m) => self.route_editor(m), // hides 20 lines
        }
    }
    fn route_ai(&mut self, ...) { ... }      // 1 call site, touches 5 fields
    fn route_editor(&mut self, ...) { ... }  // 1 call site, touches 3 fields
}

// Clean: inline everything, keep only the multi-use helper
impl State {
    pub fn update(&mut self, msg: Message) -> Action<...> {
        match msg {
            Message::Ai(m) => {
                // 30 lines of flat, readable logic — right here
            }
            Message::Editor(m) => {
                // 20 lines — right here
            }
        }
    }
    fn refresh(&mut self) -> Task<...> { ... }  // 4 call sites: earned
}
```

When `update` grows genuinely unwieldy (50+ arms, not 50+ lines),
split by **extracting child modules** — each with its own `State` +
`Message` + `update` — not by extracting helper methods within the
same module. Helpers add indirection; children add isolation.

---

## Mixed cases

A message that does plumbing AND triggers policy — push the plumbing
down, bubble only the policy:

```rust
// child.rs — "select this dataset" seeds a companion (plumbing)
//             and demands rows from the workspace (policy)
pub fn update(
    &mut self,
    message: Message,
    companion: &mut Companion,
) -> Action<Instruction, Message> {
    match message {
        Message::SelectDataset(id, schema) => {
            companion.seed(id.clone(), schema);               // plumbing: done
            Action::instruction(Instruction::EnsureRows(id))  // policy: bubbles
        }
    }
}
```

The parent's arm is one line of matching:

```rust
Message::Child(m) => {
    let action = self.child.update(m, &mut self.companion);
    // match Instruction::EnsureRows → dispatch demand pipeline
    action.map(Message::Child).map_instruction(...)
}
```

---

## Checklist

When reviewing or writing a TEA module:

1. Does every Instruction variant require state the child doesn't
   own? If not, push the state down as `&mut`.
2. Does `update` delegate to single-use `route_*` helpers? Inline
   them, or push state down so they vanish.
3. Does `update` wrap an `update_inner` with before/after diffing?
   Replace with per-arm refresh calls.
4. Is any private `&mut self` method called from fewer than 3 sites?
   Inline it. Extraction needs 3+ call sites, near-purity, and
   independent testability — all three, not just one.
5. Is the parent matching child instructions and mechanically setting
   fields? That's the child's work leaked into the parent.
