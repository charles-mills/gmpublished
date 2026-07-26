{
  lib,
  stdenv,
  rustPlatform,

  alsa-lib,
  copyDesktopItems,
  fontconfig,
  freetype,
  libGL,
  libx11,
  libxcb,
  libxcursor,
  libxi,
  libxkbcommon,
  libxrandr,
  makeDesktopItem,
  makeWrapper,
  pkg-config,
  udev,
  vulkan-loader,
  wayland,
}:

let
  cargoToml = builtins.fromTOML (builtins.readFile ../../Cargo.toml);

  pname = "gmpublished";
  version = cargoToml.workspace.package.version;

  desktopItem = makeDesktopItem {
    name = pname;
    desktopName = pname;
    comment = "Native Workshop Publishing Utility for Garry's Mod";
    exec = "${pname} -e %f";
    icon = pname;
    terminal = false;
    mimeTypes = [ "application/gma" ];
    categories = [
      "Utility"
      "Game"
      "Development"
    ];
  };

  # iced/winit dlopen these, so they never reach DT_NEEDED and are dropped from
  # the RPATH at link time.
  dlopenedLibs = [
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
            "public.mime-type" = "application/gma";
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
    fileset = lib.fileset.unions [
      ../../Cargo.lock
      ../../Cargo.toml
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
    pkg-config
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [ copyDesktopItems ];

  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [
    alsa-lib
    fontconfig
    freetype
    libGL
    libx11
    libxcb
    libxcursor
    libxi
    libxkbcommon
    libxrandr
    stdenv.cc.cc.lib
    udev
    vulkan-loader
    wayland
  ];

  desktopItems = lib.optionals stdenv.hostPlatform.isLinux [ desktopItem ];

  postInstall =
    lib.optionalString stdenv.hostPlatform.isLinux ''
      install -Dm0644 packaging/steam/redistributable/linux/libsteam_api.so \
        "$out/lib/libsteam_api.so"
      install -Dm0644 packaging/icons/128x128.png \
        "$out/share/icons/hicolor/128x128/apps/${pname}.png"
      install -Dm0644 packaging/linux/application-gma.xml \
        "$out/share/mime/packages/application-gma.xml"
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
      install -Dm0644 ${infoPlist} "$contents/Info.plist"

      makeWrapper "$contents/MacOS/${pname}" "$out/bin/${pname}"
    '';

  postFixup = lib.optionalString stdenv.hostPlatform.isLinux ''
    patchelf "$out/bin/${pname}" \
      --add-rpath ${lib.makeLibraryPath dlopenedLibs}
  '';

  meta = {
    description = "Native Workshop Publishing Utility for Garry's Mod";
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
