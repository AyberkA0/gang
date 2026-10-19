//! Core of GANG: running git, routing commands, project setup, and (in later
//! phases) auth, presence, locks and guards.

pub mod attributes;
pub mod auth;
pub mod claims;
pub mod clock;
pub mod commands;
pub mod config;
pub mod csharp;
pub mod doctor;
pub mod git;
pub mod guards;
pub mod ignore;
pub mod invocation;
pub mod layers;
pub mod lock_check;
pub mod lock_levels;
pub mod locks;
pub mod managed;
pub mod prefab_ids;
pub mod presence;
pub mod repo;
pub mod safety;
pub mod semantic;
pub mod setup;
pub mod store;
pub mod unity;
