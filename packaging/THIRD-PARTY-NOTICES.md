# Third-party notices

opendxfviewer itself is MIT (see `LICENSE`). The **release packages** additionally
contain files that are not ours, because a Makepad application cannot draw text
without the fonts its theme names. They are listed here with the licence each one
is distributed under.

None of this applies to the source repository, which ships no font files: building
from source pulls them from crates.io instead.

## Rust dependencies

Everything linked into the binary is permissively licensed. The Makepad crates
(`makepad-widgets`, `makepad-draw`, `makepad-platform` and the rest of that
family) are `MIT OR Apache-2.0`, as are `dxf`, `encoding_rs` and `rfd`; a few
vendored forks inside Makepad are MIT only (`makepad-rustybuzz`,
`makepad-objc-sys`) or `MIT OR Apache-2.0 OR Zlib` (`makepad-zune-*`).

Run `cargo tree` or consult each crate on crates.io for the authoritative list.

## Bundled fonts

Copied into the package from the Makepad crates that carry them, under
`makepad/<crate>/resources/`:

| File | Family | Licence |
|---|---|---|
| `IBMPlexSans-Text.ttf`, `-SemiBold`, `-Italic`, `-BoldItalic` | IBM Plex Sans | SIL Open Font License 1.1 |
| `NotoSans-Regular.ttf` | Noto Sans | SIL Open Font License 1.1 |
| `LiberationMono-Regular.ttf` | Liberation Mono | SIL Open Font License 1.1 |
| `fa-solid-900.ttf` | Font Awesome Free 6 | SIL Open Font License 1.1 (the font file); the icons themselves are CC BY 4.0 |
| `LXGWWenKaiRegular.ttf`, `LXGWWenKaiBold.ttf` (and their `.2` continuations) | LXGW WenKai | SIL Open Font License 1.1 |
| `NotoColorEmoji.ttf` | Noto Color Emoji | SIL Open Font License 1.1 |

The SIL Open Font License is at <https://openfontlicense.org>. The CJK and emoji
faces are most of the package's size; they are included because Makepad's theme
declares them as fallbacks, and a missing dependency is a startup failure rather
than a missing glyph.

The `.svg` icons under `makepad/makepad_widgets/resources/icons/` ship inside the
`makepad-widgets` crate and carry that crate's `MIT OR Apache-2.0`.

## AutoCAD Color Index

`src/aci.rs` transcribes the ACI palette from
[ezdxf](https://github.com/mozman/ezdxf), which is MIT.

DXF and AutoCAD are trademarks of Autodesk, Inc. This project is not affiliated
with Autodesk.
