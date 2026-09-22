#pragma once
#include "ob/msg.hpp"
#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <stdexcept>
#include <string>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

namespace ob {

// Shared file, not a private buffer. MAP_SHARED so feedd, the TUI, and
// headless see the same slots. Capacity is a power of two so the slot
// index is seq & (cap - 1). create=true truncates and zeros wseq, which
// drops whatever was in the ring. The WAL is the copy that survives that.
inline constexpr uint32_t kCap = 4096; // power of two

struct RingHeader {
  uint32_t magic;
  uint32_t cap;
  uint64_t wseq; // next write index (monotonic)
};

struct RingFile {
  RingHeader hdr;
  Msg slots[kCap];
};

class Ring {
 public:
  Ring(const std::string& path, bool create) : path_(path) {
    int flags = O_RDWR | (create ? O_CREAT : 0);
    fd_ = ::open(path.c_str(), flags, 0644);
    if (fd_ < 0) throw std::runtime_error(std::string("open ring: ") + std::strerror(errno));
    if (create) {
      if (::ftruncate(fd_, static_cast<off_t>(sizeof(RingFile))) != 0) {
        throw std::runtime_error("ftruncate ring");
      }
    }
    map_ = ::mmap(nullptr, sizeof(RingFile), PROT_READ | PROT_WRITE, MAP_SHARED, fd_, 0);
    if (map_ == MAP_FAILED) throw std::runtime_error("mmap ring");
    file_ = static_cast<RingFile*>(map_);
    if (create) {
      file_->hdr.magic = kMagic;
      file_->hdr.cap = kCap;
      file_->hdr.wseq = 0;
    }
    if (file_->hdr.magic != kMagic) throw std::runtime_error("bad ring magic");
  }
  ~Ring() {
    if (map_ && map_ != MAP_FAILED) ::munmap(map_, sizeof(RingFile));
    if (fd_ >= 0) ::close(fd_);
  }
  Ring(const Ring&) = delete;
  Ring& operator=(const Ring&) = delete;

  // Copy the message into the slot, then publish the new sequence.
  // A reader that sees the new wseq must already see the bytes in the slot.
  // The barrier is that ordering. There is one writer. Readers do not lock.
  uint64_t publish(Msg m) {
    m.magic = kMagic;
    m.nbytes = sizeof(Msg);
    uint64_t seq = file_->hdr.wseq;
    m.seq = seq;
    file_->slots[seq & (kCap - 1)] = m;
    // Publish the slot before advancing wseq, or a reader can observe a stale slot.
    __sync_synchronize();
    file_->hdr.wseq = seq + 1;
    return seq;
  }

  // cursor is owned by the reader. Pass the same integer every call.
  // false means "nothing new" or "this slot was overwritten, resync".
  // The seq check catches a torn slot: the index matches but the record
  // is from an older lap of the ring.
  bool consume(uint64_t& cursor, Msg& out) {
    uint64_t w = file_->hdr.wseq;
    if (cursor >= w) return false;
    // The writer has lapped this cursor. Skip to the oldest slot still present.
    if (w - cursor > kCap) cursor = w - kCap;
    out = file_->slots[cursor & (kCap - 1)];
    if (out.seq != cursor) {
      cursor = w;
      return false;
    }
    ++cursor;
    return true;
  }

  uint64_t wseq() const { return file_->hdr.wseq; }

 private:
  std::string path_;
  int fd_{-1};
  void* map_{nullptr};
  RingFile* file_{nullptr};
};

} // namespace ob
