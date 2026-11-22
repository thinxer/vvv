# KDE Plasma test guest

The demo uses an AArch64 Arch Linux disk with Plasma, a virtio-capable kernel and an initramfs. Supply the prepared files at `guest/arch-kde-arm64.img`, `guest/vmlinux-stable.elf` and `guest/initramfs-arm64.img`. Guest images and binaries are external build inputs. The scripts here prepare a per-session copy of the disk and install the example’s KScreen scaling helper at boot.

Start these in separate terminals from the workspace root after building vvv:

```sh
./target/debug/vvv serve --protocol both --session plasma --cuda --dev-login
./target/debug/vvv guest --session plasma --example kde --cuda
```

The example binds Apple and RDP to loopback by default. For trusted LAN testing, pass `-- --listen HOST_IP:5990` to the receiver. Apple uses TCP 5990 and UDP 5992/5994. RDP uses TCP 3389 and accepts development connections without credentials. Existing KDE sessions with a per-session disk can still use `vvv guest --session NAME --cuda` as shorthand for this example.

With both clients connected, resize the primary, move its cursor over text, then click **Take control** in the observer. Check that guest resolution follows the new primary and that keyboard, buttons and scrolling reach the guest. Restart the guest after a receiver restart to reconnect its virtio input devices. The example’s disk persists under its session directory.
