upstream:
upstream.overrideAttrs (old: {
  # Bun 的 v2 仅收紧解析校验，内容格式与 v1 相同；v3 改变 overrides，仍须拒绝。
  # https://github.com/oven-sh/bun/blob/main/src/install/lockfile/bun.lock.rs
  patches = (old.patches or [ ]) ++ [ ./bun2nix-lockfile-v2.diff ];
})
