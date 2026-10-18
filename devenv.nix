{ pkgs, lib, config, inputs, ... }:

let
  packageJson = builtins.fromJSON (builtins.readFile ./package.json);
  nativeStdenv =
    if pkgs.stdenv.hostPlatform.isLinux
    then pkgs.stdenvAdapters.useMoldLinker pkgs.clangStdenv
    else pkgs.clangStdenv;
  replaceRustCompiler = oldCompiler: dependencies:
    map (dependency:
      if dependency.outPath == oldCompiler.outPath then nativeStdenv.cc else dependency
    ) dependencies;
  # 只替换聚合包传播的编译器，不改变预编译 Rust 组件及其运行库。
  rustToolchain = ((inputs.rust-overlay.lib.mkRustBin { } pkgs.buildPackages)
    .fromRustupToolchainFile config.languages.rust.toolchainFile).overrideAttrs (old: {
    depsHostHostPropagated = replaceRustCompiler
      pkgs.buildPackages.stdenv.cc old.depsHostHostPropagated;
    propagatedBuildInputs = replaceRustCompiler
      pkgs.pkgsTargetTarget.stdenv.cc old.propagatedBuildInputs;
  });
  # C 模块硬编码 pkgs.stdenv；保留它的开发工具，但由当前环境统一提供编译器。
  cDevelopmentPackages = with pkgs; [
    clang-tools
    gnumake
  ] ++ lib.optional config.languages.c.lsp.enable config.languages.c.lsp.package
    ++ lib.optional (config.languages.c.debugger != null) config.languages.c.debugger
    ++ lib.optional (lib.meta.availableOn pkgs.stdenv.hostPlatform pkgs.valgrind
      && !pkgs.valgrind.meta.broken) pkgs.valgrind;
  desktopPackages = with pkgs; [
    gtk3
    webkitgtk_4_1
    libayatana-appindicator
    librsvg
    gsettings-desktop-schemas
  ];
  worktreeInit = pkgs.writeScript "stravia-worktree-init" ''
    #!${lib.getExe pkgs.python3}
    import fcntl
    import os
    from pathlib import Path
    import subprocess
    import sys
    import tempfile

    root = Path(os.environ["DEVENV_ROOT"])
    if not (root / ".git").is_file():
        raise SystemExit(0)

    worktrees = subprocess.check_output([
        "${pkgs.git}/bin/git", "-C", str(root), "worktree", "list", "--porcelain", "-z",
    ])
    main = Path(os.fsdecode(worktrees.split(b"\0", 1)[0].removeprefix(b"worktree ")))
    if root.samefile(main):
        raise SystemExit(0)

    print("Checking linked worktree initialization", file=sys.stderr, flush=True)
    # 锁文件保留原 inode，避免等待者与新进程锁住不同文件。
    with (Path(os.environ["DEVENV_STATE"]) / "worktree-init.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        for name in (".stravia-dev", "target"):
            source = main / name
            destination = root / name
            if os.path.lexists(destination) or not source.is_dir():
                continue

            # 临时副本与目标同文件系统；失败不发布半成品，也不覆盖并发创建的目标。
            with tempfile.TemporaryDirectory(prefix=".devenv-worktree-init-", dir=root) as temporary:
                staged = Path(temporary) / name
                subprocess.run([
                    "${pkgs.coreutils}/bin/cp", "--archive", "--reflink=always",
                    "--", str(source.resolve()), str(staged),
                ], check=True)
                subprocess.run([
                    "${pkgs.coreutils}/bin/mv", "--no-clobber", "--no-target-directory",
                    "--", str(staged), str(destination),
                ], check=True)
  '';
in
{
  stdenv = nativeStdenv;

  assertions = [
    {
      assertion = config.languages.javascript.bun.package.version == packageJson.engines.bun;
      message = "devenv.lock 中的 Bun 与 package.json 不一致，请选择包含所需 Bun 版本的 nixpkgs。";
    }
  ];

  packages = with pkgs; [
    go-task
    uv
    python3
    git
    pkg-config
    cmake
    openssl
    docker-client
  ] ++ cDevelopmentPackages
    ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux ([ pkgs.mold pkgs.perf ] ++ desktopPackages);

  services.postgres = {
    enable = true;
    package = pkgs.postgresql_16;
  };

  languages.c.enable = false;

  languages.rust = {
    enable = true;
    toolchainFile = ./rust-toolchain.toml;
    toolchainPackage = lib.mkForce rustToolchain;
    lsp.package = rustToolchain;
    # 不再额外注入默认 Clang wrapper；Cargo 直接使用同一套编译器和链接配置。
    clangLinker.enable = false;
  };

  languages.javascript = {
    enable = true;
    bun.enable = true;
  };

  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  env."CARGO_TARGET_${pkgs.stdenv.hostPlatform.rust.cargoEnvVarTarget}_LINKER" =
    "${nativeStdenv.cc}/bin/clang";

  # 放在实际 shell hook 中，避免 CLI 忽略初始化 task 的失败后继续启动。
  enterShell = ''
    ${worktreeInit} || exit $?
  '' + lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
    # 未安装到系统目录的 GTK schema 仍须能被桌面进程找到。
    export XDG_DATA_DIRS="${lib.makeSearchPath "share/gsettings-schemas/${pkgs.gtk3.name}" [ pkgs.gtk3 ]}:${lib.makeSearchPath "share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}" [ pkgs.gsettings-desktop-schemas ]}:''${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
  '';
}
