for i in 1 2 3 4 5 6; do
      if [ -d /Volumes/PHASEBOOT ]; then
        cp boot/build/fedora-initramfs-x86_64.img /Volumes/PHASEBOOT/initramfs.img
        sync && sleep 1
        diskutil eject disk21
        echo "Ready!"
        break
      fi
      echo "Waiting... ($i)"
      sleep 2
    done
