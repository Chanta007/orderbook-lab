#include "ob/book.hpp"
#include "ob/config.hpp"
#include "ob/ring.hpp"
#include <algorithm>
#include <atomic>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <string>
#include <thread>
#include <unistd.h>

static std::atomic<bool> g_run{true};
static void on_sig(int) { g_run = false; }

static std::string fmt_px(int64_t e8) {
  char buf[64];
  std::snprintf(buf, sizeof(buf), "%.2f", static_cast<double>(e8) / 1e8);
  return buf;
}

int main(int argc, char** argv) {
  if (argc < 2) {
    std::cerr << "usage: tui <config.json>\n";
    return 2;
  }
  std::signal(SIGINT, on_sig);
  std::signal(SIGTERM, on_sig);
  auto cfg = ob::load_config(argv[1]);
  ob::Ring ring(cfg.bus_path, false);
  ob::Book book;
  uint64_t cursor = ring.wseq();
  std::atomic<int> levels{cfg.levels};
  std::string cmd;

  std::thread input([&] {
    while (g_run) {
      std::string line;
      if (!std::getline(std::cin, line)) {
        g_run = false;
        break;
      }
      if (line == "q" || line == "quit") g_run = false;
      else if (line.rfind("levels ", 0) == 0) {
        int n = std::atoi(line.c_str() + 7);
        if (n >= 1 && n <= 20) levels = n;
      } else if (line == "help") {
        std::cerr << "commands: quit | levels N | help\n";
      }
    }
  });

  const char* green = "\033[32m";
  const char* reset = "\033[0m";
  while (g_run) {
    ob::Msg m{};
    while (ring.consume(cursor, m)) book.apply(m);
    int n = levels.load();
    auto bids = book.top_bids(static_cast<size_t>(n));
    auto asks = book.top_asks(static_cast<size_t>(n));
    std::printf("\033[H\033[2J");
    std::printf("orderbook-lab  env=%s  symbol=%s  seq=%llu  (quit | levels N)\n",
                cfg.env.c_str(), cfg.symbol.c_str(),
                static_cast<unsigned long long>(book.last_seq()));
    std::printf("%s%12s %12s    %12s %12s%s\n", green, "BID PX", "BID QTY", "ASK PX", "ASK QTY", reset);
    size_t rows = std::max(bids.size(), asks.size());
    for (size_t i = 0; i < rows; ++i) {
      std::string bp = i < bids.size() ? fmt_px(bids[i].px_e8) : "";
      std::string bq = i < bids.size() ? fmt_px(bids[i].qty_e8) : "";
      std::string ap = i < asks.size() ? fmt_px(asks[i].px_e8) : "";
      std::string aq = i < asks.size() ? fmt_px(asks[i].qty_e8) : "";
      std::printf("%s%12s %12s    %12s %12s%s\n", green, bp.c_str(), bq.c_str(), ap.c_str(), aq.c_str(), reset);
    }
    std::fflush(stdout);
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
  }
  if (input.joinable()) input.join();
  return 0;
}
