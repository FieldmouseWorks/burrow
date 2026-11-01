//! Where each dataset is mounted, from /proc/self/mountinfo, and the order
//! to mount their snapshots in.

use std::fs;

/// The datasets' mountpoints, parents before their children (a parent's
/// path is the shorter), so mounting one never hides another.
#[derive(Debug, PartialEq, Eq)]
pub struct Mounts(pub Vec<(String, String)>);

impl Mounts {
    /// Where each of `datasets` is mounted, as this mount namespace has it.
    pub fn find(datasets: &[String]) -> Result<Mounts, String> {
        let info = fs::read_to_string("/proc/self/mountinfo")
            .map_err(|e| format!("/proc/self/mountinfo: {e}"))?;
        Mounts::read(&info, datasets)
    }

    pub fn read(mountinfo: &str, datasets: &[String]) -> Result<Mounts, String> {
        let mounts: Vec<Mount> = mountinfo.lines().filter_map(Mount::parse).collect();
        let mut found = Vec::new();
        for ds in datasets {
            // The dataset's own mount, not a bind of a directory in it.
            let m = mounts
                .iter()
                .find(|m| m.source == *ds && m.root == "/")
                .ok_or_else(|| format!("{ds} isn't mounted"))?;
            let at = &m.target;
            // The backup runs from /nix/store and keeps what it needs live
            // under /run, so no snapshot may go over them.
            if at == "/"
                || "/nix/store/".starts_with(&format!("{at}/"))
                || "/run/".starts_with(&format!("{at}/"))
            {
                return Err(format!(
                    "{ds} is mounted at {at}, over the /nix/store or /run the backup runs from"
                ));
            }
            found.push((ds.clone(), at.clone()));
        }
        found.sort_by(|(da, a), (db, b)| a.len().cmp(&b.len()).then_with(|| da.cmp(db)));
        Ok(Mounts(found))
    }

    /// The mountpoints, in order.
    pub fn paths(&self) -> Vec<String> {
        self.0.iter().map(|(_, at)| at.clone()).collect()
    }
}

struct Mount {
    root: String,
    target: String,
    source: String,
}

impl Mount {
    /// One line of mountinfo: `ID PARENT MAJ:MIN ROOT TARGET OPTIONS
    /// [OPTIONAL...] - FSTYPE SOURCE SUPEROPTIONS`, with a space, tab,
    /// newline or backslash in a field written as `\NNN`.
    fn parse(line: &str) -> Option<Mount> {
        let fields: Vec<&str> = line.split(' ').collect();
        let dash = fields.iter().position(|f| *f == "-")?;
        Some(Mount {
            root: unescape(fields.get(3)?),
            target: unescape(fields.get(4)?),
            source: unescape(fields.get(dash + 2)?),
        })
    }
}

fn unescape(field: &str) -> String {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = b
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)));
        match (b[i], octal) {
            (b'\\', Some(d)) => {
                out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
                i += 4;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "\
22 1 0:21 / / rw,relatime shared:1 - zfs rpool/local/root rw,xattr,posixacl
43 22 0:39 /dev /home/dev rw,relatime shared:20 - zfs tank/data rw
40 22 0:39 / /data rw,relatime shared:20 - zfs tank/data rw,xattr,posixacl
41 40 0:40 / /data/nested rw,relatime shared:21 - zfs tank/data/nested rw,xattr
42 22 0:41 / /srv/keep\\040it rw,relatime shared:22 - zfs tank/keep rw,xattr
44 22 0:42 / /nix rw,relatime shared:23 - zfs rpool/local/nix rw
45 22 0:43 / /run rw,nosuid shared:24 master:5 - tmpfs tmpfs rw,mode=755
46 22 0:44 / /tank rw shared:25 - zfs tank rw
";

    fn find(datasets: &[&str]) -> Result<Mounts, String> {
        let datasets: Vec<String> = datasets.iter().map(|d| d.to_string()).collect();
        Mounts::read(INFO, &datasets)
    }

    #[test]
    fn datasets_are_found_where_they_are_mounted_parents_first() {
        let m = find(&["tank/data/nested", "tank/keep", "tank/data"]).unwrap();
        assert_eq!(
            m.0,
            [
                ("tank/data".to_owned(), "/data".to_owned()),
                ("tank/data/nested".to_owned(), "/data/nested".to_owned()),
                ("tank/keep".to_owned(), "/srv/keep it".to_owned()),
            ]
        );
        assert_eq!(m.paths(), ["/data", "/data/nested", "/srv/keep it"]);
        // The same length: by name.
        let m = find(&["tank", "tank/data"]).unwrap();
        assert_eq!(m.paths(), ["/tank", "/data"]);
    }

    #[test]
    fn a_dataset_must_be_mounted_and_not_over_what_the_backup_runs_from() {
        assert_eq!(
            find(&["tank/gone"]),
            Err("tank/gone isn't mounted".to_owned())
        );
        for (ds, at) in [
            ("rpool/local/root", "/"),
            ("rpool/local/nix", "/nix"),
            ("tmpfs", "/run"),
        ] {
            assert_eq!(
                find(&[ds]),
                Err(format!(
                    "{ds} is mounted at {at}, over the /nix/store or /run the backup runs from"
                ))
            );
        }
    }

    #[test]
    fn fields_are_unescaped() {
        assert_eq!(unescape("a\\040b\\011c\\012d\\134e"), "a b\tc\nd\\e");
        assert_eq!(unescape("a\\04"), "a\\04");
        assert_eq!(unescape("a\\x40"), "a\\x40");
        assert_eq!(unescape("\\"), "\\");
    }
}
