# Nix-layer tests: `nix build --file tests/nix/test.nix --no-link --impure`.
# Asserts what `writeUci` renders from the *working tree*, so commit your
# changes before relying on a green run.
{
  pkgs ?
    (builtins.getFlake "path:${toString ./../..}")
    .inputs.nixpkgs.legacyPackages.${builtins.currentSystem},
}:
let
  uci = pkgs.callPackage ../../nix { };
  withFlags = (uci.writeUci ./fixtures/runtime-flags.nix).command;
  withoutFlags = (uci.writeUci ../../example.nix).command;
  # A path with a space and a shell metacharacter: both must survive as one
  # quoted argument, never as shell syntax.
  hostileIdentity = (uci.writeUci ./fixtures/runtime-flags-hostile.nix).command;
in
pkgs.runCommand "nuci-nix-tests" { } ''
  script=${withFlags}
  grep -qF -- '--verbose' "$script" || { echo "missing --verbose"; exit 1; }
  grep -qF -- '--port 2222' "$script" || { echo "missing --port 2222"; exit 1; }
  grep -qF -- '--identity /tmp/test-id_ed25519' "$script" || { echo "missing --identity"; exit 1; }
  grep -qF -- '--watchdog-timeout' "$script" || { echo "missing --watchdog-timeout"; exit 1; }
  grep -qF -- 'nuci compile' "$script" || { echo "missing compile branch"; exit 1; }
  grep -qF -- 'nuci deploy' "$script" || { echo "missing deploy branch"; exit 1; }

  # Module flags precede the deploy passthrough, so the CLI value wins.
  # The compile branch also carries "$@", hence the last occurrence.
  v=$(grep -nF -- '--verbose' "$script" | head -1 | cut -d: -f1)
  p=$(grep -nF -- '$@' "$script" | tail -1 | cut -d: -f1)
  [ "$v" -lt "$p" ] || { echo "module flags must precede the CLI passthrough"; exit 1; }

  if grep -F -- 'nuci compile' "$script" | grep -qF -- '--verbose'; then
    echo "runtime flags leaked into the compile branch"; exit 1
  fi

  # The identity value is interpolated into a shell script, so a value carrying
  # shell metacharacters must come out quoted as a single argument.
  hostile=${hostileIdentity}
  grep -qF -- "--identity '/tmp/my key/id_ed25519; touch /tmp/pwned'" "$hostile" \
    || { echo "identity is not shell-quoted"; exit 1; }

  plain=${withoutFlags}
  grep -qF -- 'nuci deploy' "$plain" || { echo "missing deploy branch"; exit 1; }
  for flag in --verbose --port --identity; do
    if grep -qF -- "$flag" "$plain"; then
      echo "$flag injected although unset"; exit 1
    fi
  done

  touch $out
''
