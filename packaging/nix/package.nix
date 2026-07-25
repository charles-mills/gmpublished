{
  lib,
  stdenv,
  rustPlatform,

  alsa-lib,
  dbus,
  fontconfig,
  libGL,
  libx11,
  libxcb,
  libxcursor,
  libxi,
  libxkbcommon,
  makeWrapper,
  patchelf,
  pkg-config,
  vulkan-loader,
  wayland,
}:

let
  cargoToml = builtins.fromTOML (builtins.readFile ../../Cargo.toml);

  pname = "gmpublished";
  version = cargoToml.workspace.package.version;

  # Keep in step with flake.nix's devShell list.
  runtimeLibs = [
    alsa-lib
    dbus
    fontconfig
    libGL
    libx11
    libxcb
    libxcursor
    libxi
    libxkbcommon
    vulkan-loader
    wayland
  ];

  infoPlist = builtins.toFile "${pname}-Info.plist" (
    lib.generators.toPlist { escape = true; } {
      CFBundleDevelopmentRegion = "English";
      CFBundleDisplayName = pname;
      CFBundleDocumentTypes = [
        {
          CFBundleTypeExtensions = [ "gma" ];
          CFBundleTypeName = "GMA File";
          CFBundleTypeRole = "Viewer";
          LSHandlerRank = "Owner";
          LSItemContentTypes = [ "dev.charlesmills.gmpublished.gma" ];
        }
      ];
      CFBundleExecutable = pname;
      CFBundleIconFile = "icon.icns";
      CFBundleIdentifier = "dev.charlesmills.gmpublished";
      CFBundleInfoDictionaryVersion = "6.0";
      CFBundleName = pname;
      CFBundlePackageType = "APPL";
      CFBundleShortVersionString = version;
      CFBundleSignature = "????";
      CFBundleVersion = version;
      LSMinimumSystemVersion = "10.13";
      NSHighResolutionCapable = true;
      UTExportedTypeDeclarations = [
        {
          UTTypeConformsTo = [
            "public.data"
            "public.archive"
          ];
          UTTypeDescription = "Garry's Mod Addon";
          UTTypeIdentifier = "dev.charlesmills.gmpublished.gma";
          UTTypeTagSpecification = {
            "public.filename-extension" = [ "gma" ];
            "public.mime-type" = "application/x-garrys-mod-addon";
          };
        }
      ];
    }
  );
in
rustPlatform.buildRustPackage {
  inherit pname version;

  src = lib.fileset.toSource {
    root = ../..;
    # Excludes ../../.cargo on purpose: its -Ctarget-cpu=x86-64-v2 is for the
    # release artifacts only.
    fileset = lib.fileset.unions [
      ../../Cargo.lock
      ../../Cargo.toml
      ../../LICENSE
      ../../THIRD-PARTY-NOTICES.md
      ../../crates
      ../../packaging/icons
      ../../packaging/linux
      ../../packaging/macos
      ../../packaging/steam/redistributable
    ];
  };

  cargoLock.lockFile = ../../Cargo.lock;
  cargoBuildFlags = [
    "--package"
    pname
  ];
  cargoTestFlags = [
    "--package"
    pname
  ];

  strictDeps = true;

  nativeBuildInputs = [
    makeWrapper
    patchelf
    pkg-config
  ];

  buildInputs = lib.optionals stdenv.hostPlatform.isLinux (
    runtimeLibs ++ [ stdenv.cc.cc.lib ]
  );

  postInstall =
    lib.optionalString stdenv.hostPlatform.isLinux ''
      install -Dm0644 packaging/steam/redistributable/linux/libsteam_api.so \
        "$out/lib/libsteam_api.so"
      install -Dm0644 packaging/icons/32x32.png \
        "$out/share/icons/hicolor/32x32/apps/${pname}.png"
      install -Dm0644 packaging/icons/128x128.png \
        "$out/share/icons/hicolor/128x128/apps/${pname}.png"
      install -Dm0644 packaging/icons/128x128@2x.png \
        "$out/share/icons/hicolor/256x256/apps/${pname}.png"
      install -Dm0644 packaging/linux/application-gma.xml \
        "$out/share/mime/packages/application-gma.xml"
      install -Dm0644 packaging/linux/${pname}.desktop \
        "$out/share/applications/${pname}.desktop"
      install -Dm0644 LICENSE \
        "$out/share/doc/${pname}/LICENSE"
      install -Dm0644 THIRD-PARTY-NOTICES.md \
        "$out/share/doc/${pname}/THIRD-PARTY-NOTICES.md"
    ''
    + lib.optionalString stdenv.hostPlatform.isDarwin ''
      app="$out/Applications/${pname}.app"
      contents="$app/Contents"

      mkdir -p "$contents/MacOS" "$contents/Resources"
      mv "$out/bin/${pname}" "$contents/MacOS/${pname}"
      mv "$out/lib/libsteam_api.dylib" "$contents/MacOS/libsteam_api.dylib"
      rmdir "$out/lib"
      install -Dm0644 packaging/icons/icon.icns \
        "$contents/Resources/icon.icns"
      install -Dm0644 packaging/macos/Credits.rtf \
        "$contents/Resources/Credits.rtf"
      install -Dm0644 LICENSE \
        "$contents/Resources/LICENSE"
      install -Dm0644 THIRD-PARTY-NOTICES.md \
        "$contents/Resources/THIRD-PARTY-NOTICES.md"
      install -Dm0644 ${infoPlist} "$contents/Info.plist"

      makeWrapper "$contents/MacOS/${pname}" "$out/bin/${pname}"
    '';

  # fixupPhase's `patchelf --shrink-rpath` drops runtimeLibs and build.rs's
  # $ORIGIN, since nothing DT_NEEDED resolves against them. Re-add after.
  postFixup = lib.optionalString stdenv.hostPlatform.isLinux ''
    patchelf --add-rpath "$out/lib:${lib.makeLibraryPath runtimeLibs}" \
      "$out/bin/${pname}"
  '';

  meta = {
    description = "Native Workshop publishing and addon inspection utility for Garry's Mod";
    homepage = "https://github.com/charles-mills/gmpublished";
    license = lib.licenses.gpl3Only;
    mainProgram = pname;
    platforms = [
      "x86_64-linux"
      "x86_64-darwin"
      "aarch64-darwin"
    ];
    sourceProvenance = with lib.sourceTypes; [
      fromSource
      binaryNativeCode
    ];
  };
}
