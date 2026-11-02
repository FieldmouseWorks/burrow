//! Which entry of GRUB's generated menu boots a system, as grubenv's
//! `next_entry` names it: its submenus' titles and its own, joined with `>`.
//! Read from the menu itself, so it always matches what GRUB shows.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

/// The title path ("submenu>entry") of the menu entry whose kernel command
/// line boots `system` (`init=SYSTEM/init`), or nothing when no entry does.
/// When more than one does, the last.
///
/// The menu is read a line at a time, the way NixOS writes it: a line
/// starting `submenu "` or `menuentry "` (after blanks) opens one, its title
/// what's between its first two quotes, and a line starting `}` closes the
/// entry, or else the innermost submenu. An entry titled "" isn't one.
pub fn title_of(menu: &[u8], system: &Path) -> Option<OsString> {
    let want = [b"init=", system.as_os_str().as_bytes(), b"/init "].concat();
    let mut submenus: Vec<&[u8]> = Vec::new();
    let mut entry: &[u8] = b"";
    let mut found = None;
    for line in menu.split(|&b| b == b'\n') {
        let start = unindented(line);
        if let Some(rest) = start.strip_prefix(b"submenu \"") {
            submenus.push(title(rest));
        } else if let Some(rest) = start.strip_prefix(b"menuentry \"") {
            entry = title(rest);
        } else if start.starts_with(b"}") {
            if !entry.is_empty() {
                entry = b"";
            } else {
                submenus.pop();
            }
        } else if !entry.is_empty() && contains(&[line, b" "].concat(), &want) {
            let mut path = Vec::new();
            for s in &submenus {
                path.extend_from_slice(s);
                path.push(b'>');
            }
            path.extend_from_slice(entry);
            found = Some(OsString::from_vec(path));
        }
    }
    found
}

/// A title, from just after its opening quote: up to the next quote, or the
/// end of the line.
fn title(rest: &[u8]) -> &[u8] {
    rest.split(|&b| b == b'"').next().unwrap_or_default()
}

fn contains(line: &[u8], want: &[u8]) -> bool {
    line.windows(want.len()).any(|w| w == want)
}

/// A line without the blanks it starts with: POSIX's [[:space:]] within a
/// line (space, tab, vertical tab, form feed and carriage return).
fn unindented(line: &[u8]) -> &[u8] {
    let n = line
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'\x0b' | b'\x0c' | b'\r'))
        .count();
    &line[n..]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A menu as NixOS's install-grub.pl writes one: the default on top, then
    /// every generation in a submenu, and a second profile's in one of its
    /// own.
    const MENU: &str = r#"set default=0
menuentry "NixOS - Default" --class nixos --unrestricted {
  search --set=drive1 --fs-uuid 1234
  linux ($drive1)//kernels/aaa-bzImage init=/nix/store/sys-3/init panic=10
  initrd ($drive1)//kernels/aaa-initrd
}
submenu "NixOS - All configurations" --class submenu {
menuentry "NixOS - Configuration 3 (2026-11-01 - 26.05)" --class nixos {
  linux ($drive1)//kernels/aaa-bzImage init=/nix/store/sys-3/init panic=10
}
menuentry "NixOS - Configuration 2 (2026-10-31 - 26.05)" --class nixos {
  linux ($drive1)//kernels/bbb-bzImage init=/nix/store/sys-2/init
}
	menuentry "NixOS - Configuration 22 (2026-10-01 - 26.05)" --class nixos {
  linux ($drive1)//kernels/bbb-bzImage init=/nix/store/sys-22/init
}
}
submenu "NixOS - Profile 'test'" --class submenu {
submenu "NixOS - Nested" {
menuentry "NixOS - Test 1" {
  linux ($drive1)//kernels/ccc-bzImage init=/nix/store/sys-t1/init
}
}
menuentry "NixOS - Test 2" {
  linux ($drive1)//kernels/ccc-bzImage loglevel=4 init=/nix/store/sys-t2/init
}
}
"#;

    fn of(system: &str) -> Option<String> {
        title_of(MENU.as_bytes(), Path::new(system)).map(|t| t.into_string().unwrap())
    }

    #[test]
    fn a_system_is_the_entry_that_boots_its_init_under_its_submenus() {
        assert_eq!(
            of("/nix/store/sys-2").as_deref(),
            Some("NixOS - All configurations>NixOS - Configuration 2 (2026-10-31 - 26.05)")
        );
        // An entry indented with a tab, its init last on its line.
        assert_eq!(
            of("/nix/store/sys-22").as_deref(),
            Some("NixOS - All configurations>NixOS - Configuration 22 (2026-10-01 - 26.05)")
        );
        // A closed submenu is left; an inner one nests.
        assert_eq!(
            of("/nix/store/sys-t1").as_deref(),
            Some("NixOS - Profile 'test'>NixOS - Nested>NixOS - Test 1")
        );
        assert_eq!(
            of("/nix/store/sys-t2").as_deref(),
            Some("NixOS - Profile 'test'>NixOS - Test 2")
        );
        // The default boots it too; the last that does is the one.
        assert_eq!(
            of("/nix/store/sys-3").as_deref(),
            Some("NixOS - All configurations>NixOS - Configuration 3 (2026-11-01 - 26.05)")
        );
    }

    #[test]
    fn only_its_own_init_counts_and_only_inside_an_entry() {
        // sys-2 is a prefix of sys-22, and /init must end a word.
        for system in ["/nix/store/sys", "/nix/store/sys-", "/nix/store/sys-4", "/"] {
            assert_eq!(of(system), None, "{system}");
        }
        let outside = b"linux /k init=/nix/store/x/init\nmenuentry \"\" {\n  linux /k init=/nix/store/x/init\n}\n";
        assert_eq!(title_of(outside, Path::new("/nix/store/x")), None);
        // Not a system's init: init=/nix/store/x/initrd.
        let longer = b"menuentry \"A\" {\n  linux /k init=/nix/store/x/initrd\n}\n";
        assert_eq!(title_of(longer, Path::new("/nix/store/x")), None);
        assert_eq!(title_of(b"", Path::new("/nix/store/x")), None);
    }

    #[test]
    fn a_title_runs_to_its_next_quote_or_the_end_of_its_line() {
        let menu = b"submenu \"Open\nmenuentry \"B\" {\n init=/s/init\n}\n}\n";
        assert_eq!(
            title_of(menu, Path::new("/s")),
            Some(OsString::from("Open>B"))
        );
        // Indented with any blank POSIX's [[:space:]] knows.
        for blank in [" ", "\t", "\x0b", "\x0c", "\r"] {
            let menu = format!(
                "{blank}submenu \"S\" {{\n{blank}menuentry \"E\" {{\n init=/s/init\n{blank}}}\n{blank}}}\nmenuentry \"F\" {{\n init=/t/init\n}}\n"
            );
            assert_eq!(
                title_of(menu.as_bytes(), Path::new("/s")),
                Some(OsString::from("S>E")),
                "{blank:?}"
            );
            assert_eq!(
                title_of(menu.as_bytes(), Path::new("/t")),
                Some(OsString::from("F")),
                "{blank:?}"
            );
        }
        // A title that isn't UTF-8 is the menu's bytes, as GRUB matches them.
        let menu = b"menuentry \"\xff\" {\n init=/s/init\n}\n";
        assert_eq!(
            title_of(menu, Path::new("/s")),
            Some(OsString::from_vec(vec![0xff]))
        );
    }
}
