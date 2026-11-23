//! Conductor as a library, so the binary and the tests share it.

pub mod admin;
pub mod api;
pub mod auth;
pub mod backup;
pub mod brand;
pub mod catalog;
pub mod import;
pub mod collab;
pub mod concerts;
pub mod config;
pub mod db;
pub mod import_nd;
pub mod jam;
pub mod library;
pub mod loudness;
pub mod net;
pub mod listenbrainz;
pub mod live;
pub mod mixes;
pub mod online;
pub mod player;
pub mod prelude;
pub mod scrobble;
pub mod page;
pub mod similar;
pub mod transcode;
pub mod spotify;
pub mod vpn;
pub mod wiki;

#[cfg(test)]
mod robustness;
