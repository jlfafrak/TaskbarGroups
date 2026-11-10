# GitHub CI and Releases Implementation Plan

**Goal:** Validate Windows changes and publish a versioned MSI on a matching release tag.

**Architecture:** A Windows job runs Rust checks, builds the MSI, exercises upgrades, and uploads the installer as a workflow artifact. A separate, write-enabled job publishes that verified artifact on matching version-tag pushes.

**Tech stack:** GitHub Actions, PowerShell, Rust, WiX, GitHub CLI.

1. Test the release-tag guard with matching and mismatched versions; implement the guard.
2. Add the Windows build and release workflow with least-privilege job permissions.
3. Document tagging, installer artifacts, and unsigned installer behavior.
4. Verify the guard and Rust checks locally, push the workflow and a version tag, and inspect the resulting GitHub run and release.
