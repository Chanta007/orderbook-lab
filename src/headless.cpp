#include "ob/book.hpp"
#include "ob/config.hpp"
#include "ob/ring.hpp"
#include "ob/wal.hpp"
#include <chrono>
#include <cstdlib>
#include <iostream>
#include <thread>

int main(int argc, char** argv) {
  if (argc < 3) {
    std::cerr << "usage: headless <config.json> <wait_ms>\n";
    return 2;
  }
  auto cfg = ob::load_config(argv[1]);
  int wait_ms = std::atoi(argv[2]);
  ob::Ring ring(cfg.bus_path, false);
  ob::Book book;
  uint64_t cursor = 0;
  auto end = std::chrono::steady_clock::now() + std::chrono::milliseconds(wait_ms);
  while (std::chrono::steady_clock::now() < end) {
    ob::Msg m{};
    while (ring.consume(cursor, m)) book.apply(m);
    std::this_thread::sleep_for(std::chrono::milliseconds(10));
  }
  auto wal = ob::Wal::replay(cfg.wal_path);
  std::cout << "bids=" << book.bid_count() << " asks=" << book.ask_count()
            << " seq=" << book.last_seq() << " wal=" << wal.size() << "\n";
  if (book.bid_count() == 0 || book.ask_count() == 0 || wal.empty()) return 1;
  return 0;
}
