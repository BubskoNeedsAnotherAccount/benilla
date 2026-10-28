//! Engine acoustics: a real compressible-flow model of the intake and exhaust.
//!
//! * [`gas`] - imperfect-gas thermodynamics (speed of sound follows temperature and composition);
//! * [`material`] - pipe materials: heat capacity, conductivity, emissivity, roughness, shell damping;
//! * [`grid`] - quasi-1-D Euler solver for every pipe at once, with friction and a wall temperature that evolves;
//! * [`node`] - junctions, valves, open ends (with radiation) and closed ends;
//! * [`network`] - ducts + nodes stepped together (necks / resonators are legs of a node);
//! * [`reservoir`] - cylinders, plenums and muffler chambers as 0-D volumes;
//! * [`analysis`] - FFT, spectra and resonance finding used by the validation tests and `soundlab`.

pub mod analysis;
pub mod cylinder_gas;
pub mod exhaust;
pub mod gas;
pub mod gas_exchange;
pub mod grid;
pub mod intake;
pub mod linear;
pub mod material;
pub mod network;
pub mod node;
pub mod reservoir;

#[cfg(test)]
mod validation;

pub use cylinder_gas::{valve_train, CylGeom, Valve, ValveTrain};
pub use exhaust::{Body, ExhaustInputs, ExhaustSystem, SystemReport, Tail};
pub use gas::Gas;
pub use gas_exchange::{firing_offsets, GasPath, MouthSample};
pub use grid::{DuctSpec, End, Grid};
pub use intake::{IntakeInputs, IntakeSystem};
pub use material::{PipeMaterial, Wall};
pub use network::Network;
pub use node::{Env, Node, OpenEnd, OpenMode, PipeLeg, SlugLeg, Terminal, ValveLeg};
pub use reservoir::{orifice_flow, Reservoir};
