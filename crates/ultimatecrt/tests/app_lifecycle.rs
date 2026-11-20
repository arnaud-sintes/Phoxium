//! WP2 (app loop, lifecycle, persistence): checks that need a process of their own. The current directory is
//! process wide, so this file holds a single test.

use std::time::Instant;

use ultimatecrt::params::Params;
use ultimatecrt::tube_state::{TubeState, WearHost, WearMap};
use ultimatecrt::Res;

/// A renderer whose wear map lives in memory.
struct Host {
    map: WearMap,
    resets: u32,
}

impl WearHost for Host {
    fn wear_map_size(&self) -> (u32, u32) {
        (4, 3)
    }
    fn set_wear_map(&mut self, map: &WearMap) -> Res<()> {
        self.map = map.clone();
        Ok(())
    }
    fn reset_wear(&mut self) {
        self.map = WearMap::new(4, 3);
        self.resets += 1;
    }
    fn burn_in_now(&mut self, _: f32) {}
    fn request_wear_snapshot(&mut self) {}
    fn take_wear_snapshot(&mut self) -> Res<Option<WearMap>> {
        Ok(None)
    }
    fn read_wear_map(&mut self) -> Res<Option<WearMap>> {
        Ok(Some(self.map.clone()))
    }
}

/// R-54: without a settings folder (no `APPDATA`), the burn-in controller reads no state file from the current
/// directory, copies none to `.bak` and writes none: the wear starts as a new tube and lasts for the session.
#[test]
fn without_a_settings_folder_the_current_directory_is_left_alone() {
    let dir = std::env::temp_dir().join(format!("ucrt-app-lifecycle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["default.state", "p1.state"] {
        std::fs::write(dir.join(name), b"not a wear map").unwrap();
    }
    let before = std::env::current_dir().unwrap();
    std::env::set_current_dir(&dir).unwrap();

    let t0 = Instant::now();
    let mut tube = TubeState::new(None, false, t0);
    let mut host = Host {
        map: WearMap {
            q: vec![1.0; 36],
            ..WearMap::new(4, 3)
        },
        resets: 0,
    };
    let p = Params {
        burnin_enable: true,
        ..Params::default()
    };
    tube.tick(&mut host, &p, None, t0);
    tube.tick(&mut host, &p, Some("p1"), t0);
    tube.reset(&mut host);
    tube.finish(&mut host);
    let messages = tube.take_messages();

    std::env::set_current_dir(before).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(names, ["default.state", "p1.state"], "no .bak, no new file");
    assert_eq!(host.resets, 3, "a new tube for each preset, then New tube");
    assert_eq!(host.map.max_charge(), 0.0);
    assert!(messages.iter().all(|m| !m.contains("warning")), "{messages:?}");
    assert!(
        messages.iter().any(|m| m.contains("no settings folder")),
        "{messages:?}"
    );
}
