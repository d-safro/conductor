//! Every setting from the environment (its own test program: environment variables are shared by a whole process).

use conductor::config::Config;

#[test]
fn any_setting_from_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "trial_per_day = 3\ndiscovery = [\"x\"]\n").unwrap();
    // SAFETY: this test program has this one test; nothing else reads the environment meanwhile
    unsafe {
        std::env::set_var("CONDUCTOR_TRIAL_PER_DAY", "9"); // a number, over the file's
        std::env::set_var("CONDUCTOR_LIDARR_MATCH_RATIO", "0.5");
        std::env::set_var("CONDUCTOR_SPOTIFY_SYNC", "true");
        std::env::set_var("CONDUCTOR_DISCOVERY", "weekly-jams, daily-jams"); // a list, comma-separated
        std::env::set_var("CONDUCTOR_VPN_CHECK_KEY", "123456"); // looks like a number, is text
        std::env::set_var("CONDUCTOR_PROXY", "socks5h://127.0.0.1:1080");
        std::env::set_var("MAESTRO_INFO_LANG", "de"); // the old name still works…
        std::env::set_var("MAESTRO_LRCLIB_URL", "http://old.example");
        std::env::set_var("CONDUCTOR_LRCLIB_URL", "http://new.example"); // …and the new one wins
        std::env::set_var("CONDUCTOR_NOT_A_SETTING", "whatever");
    }
    let c = Config::load(Some(cfg.clone())).unwrap();
    assert_eq!(c.trial_per_day, 9);
    assert_eq!(c.lidarr_match_ratio, 0.5);
    assert!(c.spotify_sync);
    assert_eq!(c.discovery, ["weekly-jams", "daily-jams"]);
    assert_eq!(c.vpn_check_key.as_deref(), Some("123456"));
    assert_eq!(c.proxy.as_deref(), Some("socks5h://127.0.0.1:1080"));
    assert_eq!(c.info_lang, "de");
    assert_eq!(c.lrclib_url, "http://new.example");
    // a value the setting can't take is named
    unsafe { std::env::set_var("CONDUCTOR_TRIAL_HOURS", "soon") };
    let e = Config::load(Some(cfg)).unwrap_err();
    assert!(format!("{e:#}").contains("CONDUCTOR_TRIAL_HOURS"), "{e:#}");
}
