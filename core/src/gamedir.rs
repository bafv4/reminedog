//! Locating the game directory and reminedog's files inside it.

use std::path::{Path, PathBuf};

use crate::world::WorldId;

/// Finds the game directory from the process arguments.
///
/// Uses the last `--gameDir <path>` or `--gameDir=<path>` (the vanilla launcher passes it),
/// resolving a relative path against `cwd`. Without one, `cwd` is the game directory, which is
/// how Prism Launcher starts instances.
pub fn detect_game_dir(args: &[String], cwd: &Path) -> PathBuf {
    let mut found: Option<&str> = None;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--gameDir" {
            // A following option means the value is missing; leave it to be read as an option.
            if let Some(value) = iter.next_if(|v| !v.starts_with("--")) {
                found = Some(value);
            }
        } else if let Some(value) = arg.strip_prefix("--gameDir=") {
            found = Some(value);
        }
    }
    match found.filter(|v| !v.is_empty()) {
        Some(dir) => cwd.join(dir),
        None => cwd.to_owned(),
    }
}

/// `<game_dir>/reminedog`: everything reminedog writes lives here.
pub fn data_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("reminedog")
}

/// `<game_dir>/reminedog/waypoints/<file_stem>.json`.
pub fn waypoints_path(game_dir: &Path, world: &WorldId) -> PathBuf {
    data_dir(game_dir)
        .join("waypoints")
        .join(format!("{}.json", world.file_stem()))
}

/// `<game_dir>/saves`: singleplayer world folders.
pub fn saves_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("saves")
}

/// `<game_dir>/logs/latest.log`: the game's current log.
pub fn latest_log_path(game_dir: &Path) -> PathBuf {
    game_dir.join("logs").join("latest.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn absolute(tail: &str) -> PathBuf {
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        Path::new(root).join(tail)
    }

    #[test]
    fn falls_back_to_cwd() {
        let cwd = absolute("instances/Survival/minecraft");
        assert_eq!(detect_game_dir(&[], &cwd), cwd);
        assert_eq!(
            detect_game_dir(
                &args(&["java", "-Xmx4G", "net.minecraft.client.main.Main"]),
                &cwd
            ),
            cwd
        );
    }

    #[test]
    fn separate_and_joined_forms() {
        let cwd = absolute("cwd");
        let game = absolute("Users/me/My Games/.minecraft");
        let game_str = game.to_str().unwrap();
        assert_eq!(
            detect_game_dir(
                &args(&["Main", "--username", "x", "--gameDir", game_str]),
                &cwd
            ),
            game
        );
        assert_eq!(
            detect_game_dir(&args(&["Main", &format!("--gameDir={game_str}")]), &cwd),
            game
        );
    }

    #[test]
    fn last_occurrence_wins() {
        let cwd = absolute("cwd");
        let first = absolute("first");
        let second = absolute("second");
        let list = args(&[
            "--gameDir",
            first.to_str().unwrap(),
            &format!("--gameDir={}", second.to_str().unwrap()),
        ]);
        assert_eq!(detect_game_dir(&list, &cwd), second);
        let list = args(&[
            &format!("--gameDir={}", second.to_str().unwrap()),
            "--gameDir",
            first.to_str().unwrap(),
        ]);
        assert_eq!(detect_game_dir(&list, &cwd), first);
    }

    #[test]
    fn relative_paths_are_joined_onto_cwd() {
        let cwd = absolute("launcher");
        assert_eq!(
            detect_game_dir(&args(&["--gameDir", "instances/a"]), &cwd),
            cwd.join("instances/a")
        );
        assert_eq!(
            detect_game_dir(&args(&["--gameDir=."]), &cwd),
            cwd.join(".")
        );
    }

    #[test]
    fn missing_or_empty_values_are_ignored() {
        let cwd = absolute("cwd");
        assert_eq!(detect_game_dir(&args(&["--gameDir"]), &cwd), cwd);
        assert_eq!(detect_game_dir(&args(&["--gameDir="]), &cwd), cwd);
        assert_eq!(
            detect_game_dir(&args(&["--gameDir", "--assetsDir", "assets"]), &cwd),
            cwd
        );
        // Other options that merely resemble it do not count.
        assert_eq!(
            detect_game_dir(&args(&["--gamedir", "x", "-gameDir", "y"]), &cwd),
            cwd
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths() {
        let cwd = PathBuf::from(r"C:\Users\me\AppData\Roaming\PrismLauncher\instances\a\minecraft");
        assert_eq!(
            detect_game_dir(&args(&["--gameDir", r"D:\Games\My Minecraft"]), &cwd),
            PathBuf::from(r"D:\Games\My Minecraft")
        );
        assert_eq!(
            detect_game_dir(&args(&["--gameDir", r"..\b\minecraft"]), &cwd),
            cwd.join(r"..\b\minecraft")
        );
    }

    #[test]
    fn file_locations() {
        let game = absolute("mc");
        assert_eq!(data_dir(&game), game.join("reminedog"));
        assert_eq!(saves_dir(&game), game.join("saves"));
        assert_eq!(latest_log_path(&game), game.join("logs").join("latest.log"));
        let world = WorldId::Multiplayer {
            host: "Example.com".into(),
            port: 25565,
            label: None,
        };
        assert_eq!(
            waypoints_path(&game, &world),
            game.join("reminedog")
                .join("waypoints")
                .join("mp-example.com-25565.json")
        );
        let world = WorldId::Singleplayer {
            folder: "New World".into(),
        };
        assert_eq!(
            waypoints_path(&game, &world).file_name().unwrap(),
            "sp-New World.json"
        );
    }
}
