# Development Practices

## Negative Space Programming

The principle: prove correctness by the ABSENCE of crashes. Every function asserts its contract. If the assertion doesn't fire, the code is correct. If it fires, we catch the bug immediately — not after silent corruption.

### Where to Assert

1. **Function entry (preconditions)**:
   ```rust
   assert!(sample_rate > 0, "sample_rate must be > 0, got {}", sample_rate);
   assert!(!url.is_empty(), "from_url called with empty URL");
   assert!(max_wav_bytes > 0, "max_wav_bytes must be positive");
   ```

2. **After computation (postconditions)**:
   ```rust
   assert!(limit > 0, "max_file_bytes produced zero for {:?}", self);
   assert!(total > 0, "split_at_silence returned zero chunks");
   assert!(output.iter().all(|s| s.is_finite()), "NaN in output");
   ```

3. **At state transitions (invariants)**:
   ```rust
   assert!(self.frame_buf.len() <= DENOISE_FRAME_SIZE);
   assert!(self.silence_frames <= 1_000_000, "overflow");
   ```

4. **Compile-time guards** (const assertions):
   ```rust
   const { assert!(TARGET_RMS > 0.0 && TARGET_RMS <= 1.0); }
   ```

### Rules
- Use `assert!()`, NOT `debug_assert!()` — assertions must run in release
- Include context in message: the value that failed, which function, why
- Don't assert things that can legitimately happen (use Result for that)
- DO assert things that indicate logic bugs or corrupted state

## Test-Driven Development

### For New Features
1. Write test that describes the desired behavior
2. Run test — must FAIL
3. Write minimal implementation
4. Run test — must PASS
5. Refactor if needed

### For Bug Fixes
1. Write test that reproduces the EXACT failure (use real data characteristics)
2. Run test — must FAIL (proves the test catches the bug)
3. Write the fix
4. Run test — must PASS
5. Run ALL tests — no regressions

### Test Quality Rules
- Test the BEHAVIOR, not the implementation
- Use realistic data (not just `vec![0.5; 100]`)
- Include edge cases: empty input, max values, NaN, boundary conditions
- Regression tests must have comments explaining which bug they prevent
- Naming: `test_name_describes_what_is_tested` (e.g., `no_nan_in_output_after_silence_gap`)

## Code Quality Rules

### Audio/DSP Code
- Always clamp to [-1.0, 1.0] before AND after processing
- Check for NaN/Inf after every DSP operation (filter, AGC, denoise)
- Never assume library behavior — verify with tests (e.g., dagc NaN bug)
- Use typed pipeline (RawAudio → ProcessedAudio → WavPayload)

### API/Network Code
- Validate URLs before use (Provider::validate_url)
- Scrub API keys from error messages (Provider::scrub_api_key)
- Truncate error bodies to 200 chars
- Scale timeouts with payload size
- Always use HTTPS (except localhost)

### State Management
- All state in Mutex<T> — always handle poison errors
- Never hold multiple locks simultaneously (deadlock risk)
- Config persists non-sensitive data; keyring for API keys

## Pre-Push Checklist

```bash
cd src-tauri
cargo fmt --check
cargo clippy -- -D warnings
cargo test --lib
```

CI treats ALL clippy warnings as errors. Always run this before pushing.

### Native UI Tests (platform-specific)

Native UI builds are platform-specific; CI handles cross-platform builds automatically. To run tests locally on your platform:

- **Windows**: `dotnet test` in `native-ui/windows/`
- **macOS**: `xcodebuild test` in `native-ui/macos/`
- **Linux**: `cargo test` in `native-ui/linux/`
