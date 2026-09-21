#pragma once
#include "ob/msg.hpp"
#include <algorithm>
#include <cstdint>
#include <map>
#include <mutex>
#include <vector>

namespace ob {

struct Level {
  int64_t px_e8{0};
  int64_t qty_e8{0};
};

class Book {
 public:
  void apply(const Msg& m) {
    std::lock_guard<std::mutex> g(mu_);
    if (m.type != Type::Depth) return;
    auto& side = (m.side == Side::Bid) ? bids_ : asks_;
    if (m.qty_e8 == 0) side.erase(m.px_e8);
    else side[m.px_e8] = m.qty_e8;
    last_seq_ = m.seq;
  }
  std::vector<Level> top_bids(size_t n) const {
    std::lock_guard<std::mutex> g(mu_);
    std::vector<Level> v;
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
