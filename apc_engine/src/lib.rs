//! AuTHRity APC Engine Library
//! 
//! Core DMC controller library providing:
//! - DMC algorithm implementation
//! - Model loading and configuration
//! - OPC UA communication utilities

pub mod config;
pub mod control_authority;
pub mod monitoring;
pub mod live_forecast;
pub mod dmc;
pub mod modelloader;
pub mod opc_interface;
pub mod twin;
pub mod validation;
pub mod campaign;
pub mod campaign_plant;
pub mod campaign_transport;
pub mod native_lifecycle;
pub mod native_handoff;
pub mod replay_authority;
pub mod native_approval;
