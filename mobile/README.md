# Smelt Mobile

Flutter client for Smelt remote ACP conversations and live terminal sessions.

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

Release does not load developer overrides. App Store signing remains
intentionally unconfigured until the production Apple Developer team and final
Bundle ID are chosen.

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

`./.ci/android/env-setup.sh` installs all of the above into
`~/.smelt-ci/toolchains` without touching system directories, if you prefer not
to set them up by hand.

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
APK is not distributable. On CI, pass the same values through
`SMELT_ANDROID_KEYSTORE` and friends and let `./.ci/android/ci.sh build` write
the file; it is removed again when the build finishes.

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
