# 1. Build Bonsai in Rust with GPUI and gpuicn

Status: Proposed
Date: 2026-11-15

## Context

The SwiftUI app on the `stable` branch (v0.1.3) proved the product but grew
toward feature parity with Fork, spread across ~300 specs. Arya decided on
2026-11-15 to rebuild in Rust with GPUI, using the Karya Labs component library
gpuicn and the gpui-icons Lucide set. gpuicn v0.11.11 targets GPUI Kit 0.9.1 on
`gpui-pre` 0.3.12 and Rust 1.98.

GPUI Kit 0.9.1 already ships headless or styled building blocks Bonsai needs:
a virtualized read-only `diff` view (with conflict decorations), `tree`,
`table`, `dock`, `native_menu`, a syntax `highlighter` and a text editor.
gpuicn ships Nova-styled Sidebar, Resizable, Virtual List, Command, Menus,
Dialogs, Toasts, Tabs and form controls, but not yet Tree, Diff or a commit
graph.

gpuicn's own plan (`docs/internal/plans/desktop-components.md`) sets the rule:
missing reusable UI is built, tested and released in gpuicn before Bonsai
adopts it, and Git execution, parsing, graph layout and provider APIs stay in
Bonsai.

## Decision

- Bonsai is a Cargo workspace on Rust 1.98, edition 2024, pinned to the
  gpuicn release's GPUI Kit and `gpui-pre` versions. Upgrades move all three
  together through `gpuicn update`.
- Use gpuicn in **crate mode** (`gpuicn init --crate`). Bonsai imports
  `ui::…`; a component is ejected only for a reviewed, Bonsai-specific reason.
  Anything reusable goes upstream to gpuicn instead.
- Missing components (Tree, Diff, Commit graph) are gpuicn issues that block
  the Bonsai issues that need them. They wrap GPUI Kit's `tree` and `diff`
  where those hold up rather than starting from scratch.
- macOS only for the first milestone. Keep platform code behind small seams so
  Linux and Windows stay possible.

## Consequences

- Bonsai's UI work is partly gpuicn work; the milestone plan lists those issues
  in the gpuicn repository.
- Crate mode keeps Bonsai close to gpuicn releases; ejected files show up in
  `gpuicn status` and must be justified in review.
- The `stable` branch stays as a feature reference, not a parity target.
