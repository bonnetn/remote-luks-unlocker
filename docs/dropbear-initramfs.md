# Debian/Ubuntu Dropbear initramfs setup

This is one practical way to unlock an encrypted root filesystem over SSH
while a Debian or Ubuntu machine is still in the initramfs. It assumes
`initramfs-tools` and `cryptsetup-initramfs` are already the boot stack. It is
not a partitioning guide, and it does not replace a tested recovery plan.

Read the version installed on the target as well as this guide. The package
paths and defaults can differ between releases:

- [Debian cryptsetup initramfs guide](https://cryptsetup-team.pages.debian.net/cryptsetup/README.initramfs.html)
- [Debian remote root unlock notes](https://cryptsetup-team.pages.debian.net/cryptsetup/README.Debian.html#_remotely_unlock_encrypted_rootfs)
- [Dropbear initramfs package README](https://sources.debian.org/src/dropbear/2018.76-5%2Bdeb10u1/debian/README.initramfs/)

## Before changing the server

Have tested console or provider recovery access. The server should already
boot locally with its LUKS passphrase, have an unencrypted `/boot`, and have a
network interface that can come up before root is unlocked. Keep the private
SSH key on the client machine.

Do the first test with an out-of-band console open. A bad initramfs can leave a
remote machine unreachable.

## Install the packages

```sh
sudo apt update
sudo apt install cryptsetup-initramfs dropbear-initramfs initramfs-tools
```

`dropbear-initramfs` puts a small SSH server in the initramfs. Password login
is disabled by the package; public keys come from its initramfs configuration.
Check the actual paths on the target instead of assuming they match another
release:

```sh
dpkg -L dropbear-initramfs | less
```

The Debian package documents `/etc/dropbear-initramfs/` and
`/etc/dropbear-initramfs/config`. If the installed package uses another
directory, use that directory in the commands below.

## Add a restricted SSH key

Create a key on the client. Use a key dedicated to this initramfs role:

```sh
ssh-keygen -t ed25519 -f "$HOME/.ssh/remote-luks" -C remote-luks
```

Put the public key in the package's `authorized_keys` file. For Debian's
`cryptroot-unlock` helper, a restricted entry looks like this:

```text
no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty,command="/bin/cryptroot-unlock" ssh-ed25519 AAAA... remote-luks
```

Replace `AAAA...` with the complete contents of
`$HOME/.ssh/remote-luks.pub`. Do not wrap or edit the key body. Check the
helper path with `command -v cryptroot-unlock` on the running system and in the
generated initramfs; change the forced command if the path differs.

Protect the file and its parent:

```sh
sudo chown root:root /etc/dropbear-initramfs/authorized_keys
sudo chmod 600 /etc/dropbear-initramfs/authorized_keys
sudo chmod 700 /etc/dropbear-initramfs
```

Dropbear rejects keys when the authorized-key file or its parent is writable
by an unintended user. The forced command matters: an initramfs login is a
privileged pre-root environment.

## Configure the network and port

Choose a management-only port if the normal SSH service uses port 22. In the
Dropbear initramfs config, set the package's documented option:

```text
DROPBEAR_OPTIONS="-p 2222"
```

The initramfs also needs networking before Dropbear starts. Configure the
kernel command line using the network setup appropriate to the host. DHCP is
commonly expressed as `ip=dhcp`; a static setup needs the complete `ip=` form
for the interface, address, gateway, and nameserver. See the
[initramfs-tools configuration](https://manpages.debian.org/bookworm/initramfs-tools-core/initramfs.conf.5.en.html)
before choosing a value. Do not paste a static address from another network.

If the network driver is not included automatically, add the required module
to `/etc/initramfs-tools/modules`. Confirm the module name on the running
kernel. The Dropbear package documentation calls this out as a common reason
for an SSH service that never starts.

## Verify the initramfs host key

The initramfs has its own Dropbear host keys. Keep them separate from the
normal operating-system SSH keys. Get the fingerprint through a trusted
console or provider channel before trusting it on the client.

Once the initramfs endpoint is available, collect its key into a dedicated
file and compare the fingerprint with the trusted value:

```sh
mkdir -p "$HOME/.config/remote-luks"
ssh-keyscan -p 2222 server.example.com \
  > "$HOME/.config/remote-luks/known_hosts"
ssh-keygen -lf "$HOME/.config/remote-luks/known_hosts"
chmod 600 "$HOME/.config/remote-luks/known_hosts"
```

`ssh-keyscan` retrieves a key; it does not authenticate the key. Never fix a
changed-key error with `StrictHostKeyChecking=no` in production.

## Rebuild and test

Rebuild every installed initramfs after changing Dropbear, the authorized key,
network modules, or crypttab-related configuration:

```sh
sudo update-initramfs -u -k all
```

Inspect the generated image with the tools available on the distribution.
Then reboot with console access and confirm that the initramfs SSH endpoint
appears before root is unlocked. From the client:

```sh
remote-luks-unlocker \
  --host server.example.com \
  --port 2222 \
  --user root \
  --identity-file "$HOME/.ssh/remote-luks" \
  --known-hosts "$HOME/.config/remote-luks/known_hosts" \
  --command cryptroot-unlock \
  --luks-password "$REMOTE_LUKS_LUKS_PASSWORD"
```

The client does not choose the encrypted device. `cryptroot-unlock` delegates
that to the initramfs and `/etc/crypttab`. For non-standard layouts, consult
the cryptsetup documentation and test each mapping manually.

## Security and recovery

- Restrict the initramfs SSH port to a management VLAN, VPN, or firewall source
  range.
- Keep the private key and LUKS passphrase off the server and out of source
  control. The client needs the passphrase in memory; command-line and
  environment delivery can be visible to local administrators and tooling.
- Keep a console-tested recovery method and a recovery passphrase. Do not
  assume a successful SSH login proves the next reboot will work.
- An unencrypted `/boot`, kernel, initramfs, firmware, or bootloader remains
  outside LUKS's protection.
- Update Dropbear and the base distribution through their normal security
  channels. See the [Dropbear project](https://github.com/mkj/dropbear) and
  [Ubuntu security guidance](https://ubuntu.com/server/docs/how-to/security/)
  for broader context.

## Common failures

- **No SSH listener:** check the network boot argument, NIC driver, initramfs
  rebuild, and console output.
- **Public-key authentication rejected:** check the complete key, permissions,
  username, and the package's actual authorized-key path.
- **Host-key mismatch:** the initramfs and normal system keys are different;
  use the separately verified initramfs key.
- **Command succeeds but boot does not continue:** check the `crypttab`
  mapping and whether more than one device needs unlocking.
- **Passphrase rejected:** confirm it at the console and ensure the remote
  command reads from standard input.
