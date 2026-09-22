#pragma once
#include <cstdlib>
#include <fstream>
#include <sstream>
#include <stdexcept>
#include <string>

namespace ob {

// Tiny JSON reader for the flat config files. It is not a general parser.
// Prod will not load unless ORDERBOOK_ALLOW_PROD=1. Dev will not load if
// allow_orders is true. Those two checks are the whole safety policy.
struct Config {
  std::string env{"dev"};
  std::string bus_path{"var/dev/bus"};
  std::string wal_path{"var/dev/wal.bin"};
  std::string log_path{"var/dev/async.log"};
  std::string listen_host{"127.0.0.1"};
  int listen_port{9001};
  std::string symbol{"BTCUSDT"};
  bool allow_orders{false};
  int levels{5};
};

inline std::string json_str(const std::string& body, const std::string& key, const std::string& def) {
  auto needle = "\"" + key + "\"";
  auto p = body.find(needle);
  if (p == std::string::npos) return def;
  p = body.find(':', p);
  if (p == std::string::npos) return def;
  p = body.find_first_not_of(" \t\n", p + 1);
  if (p == std::string::npos) return def;
  if (body[p] == '"') {
    auto e = body.find('"', p + 1);
    if (e == std::string::npos) return def;
    return body.substr(p + 1, e - p - 1);
  }
  auto e = body.find_first_of(",}\n", p);
  auto s = body.substr(p, e - p);
  while (!s.empty() && (s.back() == ' ' || s.back() == '\t')) s.pop_back();
  return s;
}

inline Config load_config(const std::string& path) {
  std::ifstream in(path);
  if (!in) throw std::runtime_error("cannot read config " + path);
  std::ostringstream ss;
  ss << in.rdbuf();
  auto body = ss.str();
  Config c;
  c.env = json_str(body, "env", "dev");
  c.bus_path = json_str(body, "bus_path", c.bus_path);
  c.wal_path = json_str(body, "wal_path", c.wal_path);
  c.log_path = json_str(body, "log_path", c.log_path);
  c.listen_host = json_str(body, "listen_host", c.listen_host);
  c.listen_port = std::atoi(json_str(body, "listen_port", "9001").c_str());
  c.symbol = json_str(body, "symbol", c.symbol);
  c.allow_orders = json_str(body, "allow_orders", "false") == "true";
  c.levels = std::atoi(json_str(body, "levels", "5").c_str());
  if (c.env == "prod") {
    const char* allow = std::getenv("ORDERBOOK_ALLOW_PROD");
    if (!allow || std::string(allow) != "1") {
      throw std::runtime_error("prod config refused (set ORDERBOOK_ALLOW_PROD=1)");
    }
  }
  if (c.env == "dev" && c.allow_orders) {
    throw std::runtime_error("dev config must not set allow_orders");
  }
  return c;
}

} // namespace ob
