#pragma once
#include "ob/msg.hpp"
#include <fcntl.h>
#include <stdexcept>
#include <string>
#include <unistd.h>
#include <vector>

namespace ob {

// Append-only copy of every published Msg. The ring is overwritten after
// 4096 records. Replay reads this file. fsync every 32 appends so a crash
// loses at most that many records. macOS has no fdatasync.
class Wal {
 public:
  explicit Wal(const std::string& path, bool create) {
    int flags = O_RDWR | (create ? O_CREAT : 0);
    fd_ = ::open(path.c_str(), flags, 0644);
    if (fd_ < 0) throw std::runtime_error("open wal");
    if (create) ::lseek(fd_, 0, SEEK_END);
  }
  ~Wal() {
    if (fd_ >= 0) {
      ::fsync(fd_);
      ::close(fd_);
    }
  }
  void append(const Msg& m) {
    const char* p = reinterpret_cast<const char*>(&m);
    ssize_t n = ::write(fd_, p, sizeof(Msg));
    if (n != static_cast<ssize_t>(sizeof(Msg))) throw std::runtime_error("wal write");
    ++since_sync_;
    if (since_sync_ >= 32) {
#if defined(__APPLE__)
      ::fsync(fd_);
#else
      ::fdatasync(fd_);
#endif
      since_sync_ = 0;
    }
  }
  void sync() {
#if defined(__APPLE__)
    ::fsync(fd_);
#else
    ::fdatasync(fd_);
#endif
    since_sync_ = 0;
  }

  static std::vector<Msg> replay(const std::string& path) {
    std::vector<Msg> out;
    int fd = ::open(path.c_str(), O_RDONLY);
    if (fd < 0) return out;
    Msg m{};
    while (::read(fd, &m, sizeof(Msg)) == static_cast<ssize_t>(sizeof(Msg))) {
      if (valid(m)) out.push_back(m);
    }
    ::close(fd);
    return out;
  }

 private:
  int fd_{-1};
  int since_sync_{0};
};

} // namespace ob
