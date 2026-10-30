# Grows by a MiB at a time until it's killed. Between steps it reads its
# own 4 MiB file back, so its cgroup always has page cache to reclaim:
# oomd only acts on pressure that comes with reclaim.
{ pkgs }:
pkgs.writeCBin "thrash" ''
  #include <fcntl.h>
  #include <stdlib.h>
  #include <string.h>
  #include <unistd.h>
  static char buf[1 << 20];
  int main(void) {
    int fd = open("fill", O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return 1;
    memset(buf, 1, sizeof buf);
    for (int i = 0; i < 4; i++)
      if (write(fd, buf, sizeof buf) != sizeof buf) return 1;
    fsync(fd);
    for (;;) {
      char *p = malloc(1 << 20);
      if (!p) return 1;
      memset(p, 1, 1 << 20);
      for (off_t o = 0; pread(fd, buf, sizeof buf, o) > 0; o += sizeof buf)
        ;
    }
  }
''
