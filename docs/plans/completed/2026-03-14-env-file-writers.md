# Env File Writers for External Services

## Overview
- Create an `env` module that generates environment files for external services (mavlink-routerd, camera) on unitctl startup
- Each env file writer is a separate struct implementing the `Task` trait, spawning a tokio task that writes the file and exits
- Mavlink env values are sourced from existing `[mavlink]` config section (plus new `gcs_ip` and `env_path` fields)
- Camera env values come from a new `[camera]` config section (all values in config.toml)
- Write-on-start only: files are written once at startup, not updated during runtime

## Context (from discovery)
- Files/components involved: `src/config.rs`, new `src/env/` module, `config.toml.example`
- Related patterns: `Task` trait in `main.rs` (`run() -> Vec<JoinHandle>`), sensor module structure
- Dependencies: no new crates needed — `std::fs::write` suffices for env file generation

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility

## Testing Strategy
- **Unit tests**: required for every task (see Development Approach above)
- Test env file content generation (correct KEY=VALUE format)
- Test config parsing with new fields
- Test config validation for new fields

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with + prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope
- Keep plan in sync with actual work done

## What Goes Where
- **Implementation Steps** (`[ ]` checkboxes): tasks achievable within this codebase - code changes, tests, documentation updates
- **Post-Completion** (no checkboxes): items requiring external action - manual testing, changes in consuming projects, deployment configs, third-party verifications

## Implementation Steps

### Task 1: Add mavlink env config fields to config.rs
- [x] Add `gcs_ip` (String) and `env_path` (String) fields to `MavlinkConfig`
- [x] Add validation for `env_path` (non-empty) and `gcs_ip` (non-empty, no dash prefix) in `Config::validate()`
- [x] Update `FULL_TEST_CONFIG` constant and `test_config()` with new fields
- [x] Update all existing test TOML strings that parse `MavlinkConfig` to include new fields
- [x] Write tests for new validation rules (empty env_path rejected, empty gcs_ip rejected)
- [x] Run tests - must pass before next task

### Task 2: Add camera config section to config.rs
- [x] Add `CameraConfig` struct with fields: `gcs_ip` (String), `env_path` (String), `remote_video_port` (u16), `width` (u32), `height` (u32), `framerate` (u32), `bitrate` (u32), `flip` (u8), `camera_type` (String), `device` (String)
- [x] Add `camera` field to top-level `Config` struct
- [x] Add validation for camera config (non-empty env_path, gcs_ip, camera_type, device; width/height/framerate > 0)
- [x] Update `FULL_TEST_CONFIG` constant and all test TOML strings with `[camera]` section
- [x] Write tests for camera config parsing (success case with all fields)
- [x] Write tests for camera validation (empty fields rejected, zero dimensions rejected)
- [x] Run tests - must pass before next task

### Task 3: Create env module with MavlinkEnvWriter
- [x] Create `src/env/mod.rs` declaring the module and re-exporting both writers
- [x] Create `src/env/mavlink_env.rs` with `MavlinkEnvWriter` struct holding `Arc<Context>` and `CancellationToken`
- [x] Implement `Task` for `MavlinkEnvWriter`: `run()` spawns a tokio task that writes the mavlink env file and returns
- [x] Env file format (each line `KEY=VALUE`, no quotes):
  ```
  GCS_IP={mavlink.gcs_ip}
  REMOTE_MAVLINK_PORT={mavlink.remote_mavlink_port}
  SNIFFER_SYS_ID={mavlink.sniffer_sysid}
  LOCAL_MAVLINK_PORT={mavlink.local_mavlink_port}
  FC_TTY={mavlink.fc.tty}
  FC_BAUDRATE={mavlink.fc.baudrate}
  ```
- [x] Add `mod env;` to `main.rs`
- [x] Write tests for env file content generation (verify correct KEY=VALUE output)
- [x] Write tests for file write (write to tempdir, read back, verify content)
- [x] Run tests - must pass before next task

### Task 4: Add CameraEnvWriter to env module
- [x] Create `src/env/camera_env.rs` with `CameraEnvWriter` struct holding `Arc<Context>` and `CancellationToken`
- [x] Implement `Task` for `CameraEnvWriter`: `run()` spawns a tokio task that writes the camera env file and returns
- [x] Env file format:
  ```
  GCS_IP={camera.gcs_ip}
  REMOTE_VIDEO_PORT={camera.remote_video_port}
  CAMERA_WIDTH={camera.width}
  CAMERA_HEIGHT={camera.height}
  CAMERA_FRAMERATE={camera.framerate}
  CAMERA_BITRATE={camera.bitrate}
  CAMERA_FLIP={camera.flip}
  CAMERA_TYPE={camera.camera_type}
  CAMERA_DEVICE={camera.device}
  ```
- [x] Write tests for camera env file content generation
- [x] Write tests for file write (write to tempdir, read back, verify content)
- [x] Run tests - must pass before next task

### Task 5: Wire env writers into main.rs
- [x] Instantiate `MavlinkEnvWriter` and `CameraEnvWriter` in main.rs, spawn before other tasks
- [x] Add `handles.extend()` for both env writers
- [x] Update `config.toml.example` with new mavlink fields (`gcs_ip`, `env_path`) and full `[camera]` section
- [x] Run tests - must pass before next task

### Task 6: Verify acceptance criteria
- [x] Verify all requirements from Overview are implemented
- [x] Verify edge cases are handled (missing parent directories for env_path)
- [x] Run full test suite (unit tests)
- [x] Run linter (`cargo clippy` and `cargo fmt --check`) - all issues must be fixed
- [x] Verify test coverage meets project standard

### Task 7: [Final] Update documentation
- [x] Update CLAUDE.md with env module description, new config fields, and env writer types
- [x] Update config.toml.example comments

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### Config Changes

**MavlinkConfig** (existing, add 2 fields):
- `gcs_ip: String` — GCS IP address for mavlink env file
- `env_path: String` — path to write mavlink.env

**CameraConfig** (new section):
```toml
[camera]
gcs_ip = "10.101.0.1"
env_path = "/etc/camera.env"
remote_video_port = 5600
width = 640
height = 360
framerate = 60
bitrate = 1664000
flip = 0
camera_type = "rpi"
device = "/dev/video1"
```

### Env File Format
Plain text, one `KEY=VALUE` per line, no quotes, no trailing newline after last line. Written with `std::fs::write()`.

### Task Pattern
Each writer implements `Task::run()` which spawns a single tokio task. The task writes the env file using blocking `std::fs::write` (wrapped in `tokio::task::spawn_blocking` or direct since file writes are fast), logs success/failure, and exits. The returned `JoinHandle` allows main.rs to await completion before proceeding.

## Post-Completion
*Items requiring manual intervention or external systems*

**Manual verification:**
- Deploy to target hardware and verify env files are created at expected paths
- Verify external services (mavlink-routerd, camera streamer) correctly read the generated env files
- Test with different config.toml values to ensure env files reflect changes
