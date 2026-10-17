//! Core of GANG: running git, routing commands, project setup, and (in later
//! phases) auth, presence, locks and guards.

pub mod attributes;
pub mod commands;
pub mod config;
pub mod git;
pub mod guards;
pub mod ignore;
pub mod invocation;
pub mod managed;
pub mod repo;
pub mod safety;
pub mod setup;
pub mod unity;
