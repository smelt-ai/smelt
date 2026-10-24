# Smelt Mobile

Flutter client for Smelt remote ACP conversations and live terminal sessions.

## Flutter SDK version

The Flutter version (and the CocoaPods version iOS builds use, plus the Ruby it
runs on) is pinned in the repository root `mise.toml`; CI installs from the same
file. Install [mise](https://mise.jdx.dev), activate it in your shell, then once
per checkout:

```sh
mise trust && mise install
```

Inside the repository `flutter` and `pod` then resolve to the pinned versions.

Matching it locally is not optional. Every `flutter` invocation silently
migrates the platform projects to the shape its own SDK expects — the iOS
deployment target, the engine checksum in `ios/Podfile.lock`, the exclusion
list in `analysis_options.yaml`. Running a different version rewrites those
files, and the pinned version rewrites them back, so they show up as unrelated
diffs in every commit. Check with:

```sh
flutter --version
```

When you deliberately upgrade, bump the version in `mise.toml` and commit
whatever the new SDK migrates in the same change, so the two never drift.

## iOS development signing

Debug and Profile builds load a local signing override. Create it once per
developer:

```sh
cp ios/Flutter/Developer.xcconfig.example ios/Flutter/Developer.xcconfig
```

Then set a Team ID available in your Xcode account and a unique Bundle ID
registered by that team:

```xcconfig
DEVELOPMENT_TEAM = YOUR_TEAM_ID
PRODUCT_BUNDLE_IDENTIFIER = ai.smelt.mobile.yourname
```

`Developer.xcconfig` is ignored by Git, so developers can install independent
copies without changing `project.pbxproj`. Keep the same Bundle ID to preserve
that developer build's Keychain pairing data across upgrades.

Release does not load developer overrides: it signs with the production team
and Bundle ID in `ios/Flutter/SigningDefaults.xcconfig`. Build App Store /
TestFlight packages with `make ios` from the repository root, on the release
tag; it stamps the release version (see `docs/release.md`, section 移动端).

## Android development

Requirements (the versions come from the Flutter template; `flutter doctor`
verifies most of them):

| Tool | Version |
| --- | --- |
| JDK | 17–25 (Gradle 9.1 rejects 26+) |
| Android SDK Platform | 36 |
| Android Build-Tools | 36.0.0 |
| Android NDK | 28.2.13676358 |
| Rust targets | `aarch64-linux-android` (plus `armv7-linux-androideabi`, `x86_64-linux-android`, `i686-linux-android` for other ABIs) |

```sh
flutter build apk --debug --target-platform android-arm64
```

Only `android-arm64` is built by default. Every extra ABI means another full
cross-compile of the Rust dependency tree, which dominates build time.

### Release signing

Release builds read `android/key.properties`, which is gitignored:

```properties
storeFile=/absolute/path/to/smelt-release.jks
storePassword=...
keyAlias=smelt
keyPassword=...
```

Without that file the release build falls back to the debug keystore, so
`flutter run --release` still works on a development machine — but the resulting
APK is not distributable. The release workflow writes the file from repository
secrets and removes it again when the build finishes.

The production keystore and the final Play Store listing are intentionally not
configured yet, mirroring the iOS situation above.

### Rust ↔ Gradle integration

`rust_builder/android/` drives [cargokit], which invokes the NDK to
cross-compile `crates/smelt-mobile` into `libsmelt_mobile.so` and merges the
result into the APK's `jniLibs`.

`rust_builder/cargokit/gradle/plugin.gradle` carries local changes for Gradle 9
/ AGP 9 — upstream still uses `project.exec`, `project.buildDir` and
`android.applicationVariants`, all of which have been removed. The file header
lists every deviation, and `git log` on it shows the unmodified upstream version
it started from. Re-check it after updating cargokit, since a vendored refresh
will silently overwrite those changes.

[cargokit]: https://github.com/irondash/cargokit
