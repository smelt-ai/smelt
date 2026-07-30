#
# To learn more about a Podspec see http://guides.cocoapods.org/syntax/podspec.html.
# Run `pod lib lint rust_lib_smelt_mobile.podspec` to validate before publishing.
#
Pod::Spec.new do |s|
  s.name             = 'rust_lib_smelt_mobile'
  s.version          = '0.0.1'
  s.summary          = 'Builds the smelt_mobile Rust library (iroh tunnel).'
  s.description      = <<-DESC
Builds the smelt_mobile Rust library used for the iroh P2P tunnel.
                       DESC
  s.homepage         = 'https://github.com/smelt-ai/smelt'
  s.license          = { :type => 'MIT' }
  s.author           = { 'smelt' => 'dev@smelt.ai' }

  # This will ensure the source files in Classes/ are included in the native
  # builds of apps using this FFI plugin. Podspec does not support relative
  # paths, so Classes contains a forwarder C file that relatively imports
  # `../src/*` so that the C sources can be shared among all target platforms.
  s.source           = { :path => '.' }
  s.source_files = 'Classes/**/*'
  s.dependency 'Flutter'
  s.platform = :ios, '11.0'

  # Flutter.framework does not contain a i386 slice.
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES', 'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386' }
  s.swift_version = '5.0'

  s.script_phase = {
    :name => 'Build Rust library',
    # First argument is relative path to the `rust` folder, second is name of rust library
    :script => 'sh "$PODS_TARGET_SRCROOT/../cargokit/build_pod.sh" ../../../crates/smelt-mobile smelt_mobile',
    :execution_position => :before_compile,
    :input_files => ['${BUILT_PRODUCTS_DIR}/cargokit_phony'],
    # Let XCode know that the static library referenced in -force_load below is
    # created by this build step.
    :output_files => ["${BUILT_PRODUCTS_DIR}/libsmelt_mobile.a"],
  }
  s.pod_target_xcconfig = {
    'DEFINES_MODULE' => 'YES',
    # Flutter.framework does not contain a i386 slice.
    'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386',
    'OTHER_LDFLAGS' => '-force_load ${BUILT_PRODUCTS_DIR}/libsmelt_mobile.a -framework Network -framework SystemConfiguration',
  }
  s.user_target_xcconfig = {
    # iroh 用 Network.framework 监测网络路径变化（切 Wi-Fi/蜂窝后要重新打洞），
    # 用 SystemConfiguration 读系统 DNS 配置。因为上面用 -force_load 把静态库
    # 并进了宿主 target，这两个框架也必须由宿主链接，否则报
    # Undefined symbol: _nw_path_monitor_* 。
    'OTHER_LDFLAGS' => '$(inherited) -framework Network -framework SystemConfiguration',
  }
end