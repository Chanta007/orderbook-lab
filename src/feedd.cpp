#include "ob/config.hpp"
#include "ob/ring.hpp"
#include "ob/trace.hpp"
#include "ob/wal.hpp"
#include <atomic>
#include <arpa/inet.h>
#include <csignal>
#include <cstring>
#include <iostream>
#include <netinet/in.h>
#include <sys/socket.h>
#include <thread>
#include <unistd.h>
#include <vector>

static std::atomic<bool> g_run{true};
static void on_sig(int) { g_run = false; }

static bool recv_all(int fd, char* p, size_t n) {
  size_t got = 0;
  while (got < n) {
    ssize_t r = ::recv(fd, p + got, n - got, 0);
    if (r <= 0) return false;
    got += static_cast<size_t>(r);
  }
  return true;
}

int main(int argc, char** argv) {
  if (argc < 2) {
    std::cerr << "usage: feedd <config.json>\n";
    return 2;
  }
  std::signal(SIGINT, on_sig);
  std::signal(SIGTERM, on_sig);
  auto cfg = ob::load_config(argv[1]);
  ob::Ring ring(cfg.bus_path, true);
  ob::Wal wal(cfg.wal_path, true);
  ob::AsyncLog log(cfg.log_path);

  int srv = ::socket(AF_INET, SOCK_STREAM, 0);
  int opt = 1;
  ::setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));
  sockaddr_in addr{};
  addr.sin_family = AF_INET;
  addr.sin_port = htons(static_cast<uint16_t>(cfg.listen_port));
  addr.sin_addr.s_addr = inet_addr(cfg.listen_host.c_str());
  if (::bind(srv, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) != 0) {
    std::cerr << "bind failed\n";
    return 1;
  }
  ::listen(srv, 4);
  std::cerr << "feedd listen " << cfg.listen_host << ":" << cfg.listen_port << " env=" << cfg.env << "\n";

  // accept loop in a thread so we can stop
  std::thread acc([&] {
    while (g_run) {
      fd_set rfds;
      FD_ZERO(&rfds);
      FD_SET(srv, &rfds);
      timeval tv{0, 200000};
      int sel = ::select(srv + 1, &rfds, nullptr, nullptr, &tv);
      if (sel <= 0) continue;
      int c = ::accept(srv, nullptr, nullptr);
      if (c < 0) continue;
      std::thread([c, &ring, &wal, &log, &cfg] {
        while (g_run) {
          ob::Msg m{};
          if (!recv_all(c, reinterpret_cast<char*>(&m), sizeof(m))) break;
          if (!ob::valid(m)) continue;
          if (m.symbol[0] == 0) {
            std::strncpy(m.symbol, cfg.symbol.c_str(), 7);
          }
          ob::fill_ids(m);
          m.ts_ns = ob::now_ns();
          ring.publish(m);
          wal.append(m);
          log.push(m);
        }
        ::close(c);
      }).detach();
    }
  });

  while (g_run) std::this_thread::sleep_for(std::chrono::milliseconds(50));
  ::shutdown(srv, SHUT_RDWR);
  ::close(srv);
  wal.sync();
  if (acc.joinable()) acc.join();
  return 0;
}
