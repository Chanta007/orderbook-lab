#pragma once
#include "ob/msg.hpp"
#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <mutex>
#include <queue>
#include <string>
#include <thread>

namespace ob {

inline uint64_t now_ns() {
  using namespace std::chrono;
  return duration_cast<nanoseconds>(steady_clock::now().time_since_epoch()).count();
}

// Local ids only. The first 8 bytes of the trace id are a clock. The next
// 8 are a counter. The span id repeats the counter. Say "correlation id",
// not "we export traces". Nothing leaves the process.
inline void fill_ids(Msg& m) {
  static std::atomic<uint64_t> ctr{1};
  uint64_t n = ctr.fetch_add(1, std::memory_order_relaxed);
  uint64_t t = now_ns();
  std::memset(m.trace_id, 0, 16);
  std::memcpy(m.trace_id, &t, 8);
  std::memcpy(m.trace_id + 8, &n, 8);
  std::memset(m.span_id, 0, 8);
  std::memcpy(m.span_id, &n, 8);
}

// Off the publish thread. push() drops when the queue is over 10000 so a
// slow disk cannot stall feedd. Ids are 16 and 8 bytes, the W3C sizes,
// filled locally. Nothing here talks to an OpenTelemetry collector.
class AsyncLog {
 public:
  explicit AsyncLog(std::string path) : path_(std::move(path)), run_(true) {
    th_ = std::thread([this] { loop(); });
  }
  ~AsyncLog() {
    run_.store(false);
    if (th_.joinable()) th_.join();
  }
  void push(const Msg& m) {
    std::lock_guard<std::mutex> g(mu_);
    if (q_.size() > 10000) return; // drop, never block feed
    q_.push(m);
  }

 private:
  void loop() {
    std::ofstream out(path_, std::ios::app);
    while (run_.load() || true) {
      Msg m{};
      {
        std::lock_guard<std::mutex> g(mu_);
        if (q_.empty()) {
          if (!run_.load()) break;
        } else {
          m = q_.front();
          q_.pop();
        }
      }
      if (m.magic == kMagic && out) {
        char tid[33]{};
        for (int i = 0; i < 16; ++i) std::snprintf(tid + i * 2, 3, "%02x", m.trace_id[i]);
        out << m.seq << ' ' << tid << ' ' << static_cast<int>(m.type) << ' '
            << m.px_e8 << ' ' << m.qty_e8 << '\n';
      } else {
        std::this_thread::sleep_for(std::chrono::milliseconds(2));
      }
    }
  }
  std::string path_;
  std::atomic<bool> run_;
  std::mutex mu_;
  std::queue<Msg> q_;
  std::thread th_;
};

} // namespace ob
