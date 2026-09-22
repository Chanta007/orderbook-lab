#pragma once
#include "ob/msg.hpp"
#include <algorithm>
#include <cstdint>
#include <map>
#include <mutex>
#include <vector>

namespace ob {

// WALKTHROUGH, book.hpp
//
// In-memory book. One map per side, price to size. Not on the wire.
//
// apply is the whole state machine:
//   DepthReset clears that side and returns. It does not change last_seq,
//   because a clear is not a trade or a new top price.
//   Depth with qty 0 erases that price. This is how a diff feed deletes.
//   Depth with qty > 0 inserts or replaces that price.
//   Anything else is ignored.
//
// top_bids walks the map backwards so the first row is the highest bid.
// top_asks walks forward so the first row is the lowest ask. The inside
// of the book is those two first rows. If a bid is above an ask, an old
// price was not cleared. That is a feed bug, not a sort bug.
//
// The mutex is here because the TUI applies on one thread and could later
// read from another. feedd does not touch Book.

struct Level {
  int64_t px_e8{0};
  int64_t qty_e8{0};
};

class Book {
 public:
  void apply(const Msg& m) {
    std::lock_guard<std::mutex> g(mu_);
    auto& side = (m.side == Side::Bid) ? bids_ : asks_;
    // Reset does not touch last_seq. A clear is not a new price.
    if (m.type == Type::DepthReset) {
      side.clear();
      return;
    }
    if (m.type != Type::Depth) return;
    if (m.qty_e8 == 0) side.erase(m.px_e8);
    else side[m.px_e8] = m.qty_e8;
    last_seq_ = m.seq;
  }
  std::vector<Level> top_bids(size_t n) const {
    std::lock_guard<std::mutex> g(mu_);
    std::vector<Level> v;
    // Highest bid first. Asks below walk lowest first, so the top row is the inside.
    for (auto it = bids_.rbegin(); it != bids_.rend() && v.size() < n; ++it)
      v.push_back({it->first, it->second});
    return v;
  }
  std::vector<Level> top_asks(size_t n) const {
    std::lock_guard<std::mutex> g(mu_);
    std::vector<Level> v;
    for (auto it = asks_.begin(); it != asks_.end() && v.size() < n; ++it)
      v.push_back({it->first, it->second});
    return v;
  }
  uint64_t last_seq() const {
    std::lock_guard<std::mutex> g(mu_);
    return last_seq_;
  }
  size_t bid_count() const {
    std::lock_guard<std::mutex> g(mu_);
    return bids_.size();
  }
  size_t ask_count() const {
    std::lock_guard<std::mutex> g(mu_);
    return asks_.size();
  }

 private:
  mutable std::mutex mu_;
  std::map<int64_t, int64_t> bids_;
  std::map<int64_t, int64_t> asks_;
  uint64_t last_seq_{0};
};

} // namespace ob
