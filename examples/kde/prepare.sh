# Sourced by guest/start-session.sh under the per-session guest lock.
if [[ ! -e "$state/kde.img" ]]; then
    staging=$(mktemp "$state/kde.img.XXXXXX")
    trap 'rm -f "$staging"' EXIT
    cp --reflink=auto --sparse=always guest/arch-kde-arm64.img "$staging"
    mv "$staging" "$state/kde.img"
    trap - EXIT
fi
# Append a small initramfs archive so existing guest disks also receive the KDE helper.
export CROSVM_INITRD="$state/kde-initramfs.img"
(
    staging=$(mktemp -d "$state/initrd.XXXXXX")
    trap 'rm -rf "$staging"' EXIT
    cp examples/kde/initramfs-init "$staging/init"
    chmod 755 "$staging/init"
    cp examples/kde/vvv-output-scale{,.desktop} "$staging/"
    cp --reflink=auto guest/initramfs-arm64.img "$staging/image"
    cd "$staging"
    truncate -s %4 image
    printf '%s\n' init vvv-output-scale vvv-output-scale.desktop | cpio --quiet -o -H newc >> image
    mv image "$CROSVM_INITRD"
)
export CROSVM_DISK="$state/kde.img"
