# benilla's `avian3d` fork — what differs from upstream, and how to check

Upstream: [`Jondolf/avian`](https://github.com/Jondolf/avian), MIT OR Apache-2.0, version `0.6.1`,
the version the workspace lock resolves to. Wired in through `[patch.crates-io]` in the workspace
root. The crate is copied from the cargo registry as published; only the file below differs.

## Why a fork exists at all

Avian computes a collider's mass properties when the `Collider` is inserted and again whenever it
or its `ColliderDensity` changes. For a trimesh that walks every triangle (parry's
`mass_properties_trimesh3d`, `trimesh_signed_volume_and_center_of_mass`). Streamed map colliders
are static scenery whose mass is never used, and the walk ran in the spawn frame on the main
thread: ~26 ms of a 210 ms stretch of tile and doodad streaming in a Chrome profile of the web
build (`MassProperties::from_trimesh`).

## Patch — `src/collision/collider/backend.rs`

A collider whose `ColliderDensity` is `0.0` gets `MassProperties::ZERO` without the shape walk,
both in the `Collider` `on_insert` hook and in `update_collider_mass_properties`. A density of
zero already means a mass of zero; the only thing skipped is the centre of mass and inertia a
massless shape would report, which nothing reads on a body that never moves. Benilla inserts
`ColliderDensity(0.0)` on its static map colliders (`benilla-world` `terrain_stream/collider.rs`).

## How to check

`diff -u <registry>/avian3d-0.6.1/src/collision/collider/backend.rs
third_party/avian3d/src/collision/collider/backend.rs` shows the patch and nothing else; every
other file is byte-identical to the registry crate.
