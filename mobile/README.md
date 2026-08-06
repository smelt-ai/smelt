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
