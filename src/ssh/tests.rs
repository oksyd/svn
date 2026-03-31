#![allow(clippy::unwrap_used)]

use std::time::Duration;

use super::openssh::OpenSshConfig;
use super::resolve::resolve_ssh_settings;
use super::*;

#[test]
fn resolve_ssh_username_prefers_override() {
    let url = SvnUrl::parse("svn+ssh://alice@example.com/repo").unwrap();
    let ssh = SshConfig::new(SshAuth::None).with_username("bob");
    let settings = resolve_ssh_settings(&url, &ssh, Duration::from_secs(1), None).unwrap();
    assert_eq!(settings.username, "bob");
}

#[test]
fn resolve_ssh_username_uses_url_user() {
    let url = SvnUrl::parse("svn+ssh://alice@example.com/repo").unwrap();
    let ssh = SshConfig::new(SshAuth::None);
    let settings = resolve_ssh_settings(&url, &ssh, Duration::from_secs(1), None).unwrap();
    assert_eq!(settings.username, "alice");
}

#[test]
fn openssh_config_can_override_host_user_and_port() {
    let cfg = OpenSshConfig::parse_str(
        "Host example.com\n  HostName real.example.com\n  User alice\n  Port 2222\n",
    );
    let params = cfg.query("example.com");

    let url = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
    let ssh = SshConfig::new(SshAuth::None);
    let settings =
        resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
    assert_eq!(settings.connect_host, "real.example.com");
    assert_eq!(settings.connect_port, 2222);
    assert_eq!(settings.username, "alice");
}

#[test]
fn openssh_config_does_not_override_explicit_url_port() {
    let cfg = OpenSshConfig::parse_str("Host example.com\n  Port 2222\n");
    let params = cfg.query("example.com");

    let url = SvnUrl::parse("svn+ssh://example.com:2200/repo").unwrap();
    let ssh = SshConfig::new(SshAuth::None);
    let settings =
        resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
    assert_eq!(settings.connect_port, 2200);
}

#[test]
fn openssh_config_host_key_alias_is_used_for_known_hosts_lookup() {
    let cfg = OpenSshConfig::parse_str("Host example.com\n  HostKeyAlias alias\n");
    let params = cfg.query("example.com");

    let url = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
    let ssh = SshConfig::new(SshAuth::None);
    let settings =
        resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
    assert_eq!(settings.known_hosts_host, "alias");
}

#[test]
fn openssh_config_strict_host_key_checking_no_disables_verification() {
    let cfg = OpenSshConfig::parse_str("Host example.com\n  StrictHostKeyChecking no\n");
    let params = cfg.query("example.com");

    let url = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
    let ssh = SshConfig::new(SshAuth::None);
    let settings =
        resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
    assert!(matches!(settings.host_key, SshHostKeyPolicy::AcceptAny));
    assert!(!settings.accept_new_host_keys);
}

#[test]
fn openssh_config_strict_host_key_checking_accept_new_enables_learning() {
    let cfg = OpenSshConfig::parse_str("Host example.com\n  StrictHostKeyChecking accept-new\n");
    let params = cfg.query("example.com");

    let url = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
    let ssh = SshConfig::new(SshAuth::None);
    let settings =
        resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
    assert!(settings.accept_new_host_keys);
}
