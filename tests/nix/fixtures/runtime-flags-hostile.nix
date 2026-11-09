{ ... }:
{
  uci.packageManager = "apk";
  # A path with a space and a shell metacharacter: both must be quoted when the
  # value is interpolated into the generated shell script.
  uci.identity = "/tmp/my key/id_ed25519; touch /tmp/pwned";
}
