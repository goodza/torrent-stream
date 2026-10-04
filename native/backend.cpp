// Narrow, exception-contained C ABI. Rust serializes access to each Engine.
#include <libtorrent/session.hpp>
#include <libtorrent/torrent_info.hpp>
#include <libtorrent/torrent_status.hpp>
#include <libtorrent/magnet_uri.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/ip_filter.hpp>
#include <cstdlib>
#include <cstring>
#include <sstream>
#include <stdexcept>
#include <algorithm>
#ifdef TS_INTEGRATION_TESTS
#include <libtorrent/create_torrent.hpp>
#include <libtorrent/bencode.hpp>
#include <fstream>
#include <filesystem>
#endif

namespace lt = libtorrent;
struct Engine {
    lt::session session;
    lt::torrent_handle handle;
    std::string error;
    explicit Engine(lt::settings_pack const& settings) : session(settings) {}
    void check() {
        std::vector<lt::alert*> alerts;
        session.pop_alerts(&alerts);
        for (auto* a : alerts) {
            if (auto* f = lt::alert_cast<lt::file_error_alert>(a)) error = f->message();
            if (auto* f = lt::alert_cast<lt::torrent_error_alert>(a)) error = f->message();
            if (auto* f = lt::alert_cast<lt::metadata_failed_alert>(a)) error = f->message();
        }
        if (!error.empty()) throw std::runtime_error(error);
        auto s = handle.status({});
        if (s.errc) throw std::runtime_error(s.errc.message());
    }
};
static thread_local std::string last_error;
static std::string quote(std::string const& s) {
    std::ostringstream out; out << '"';
    for (unsigned char c : s) {
        if (c == '"' || c == '\\') out << '\\' << c;
        else if (c < 32) { const char* hex = "0123456789abcdef"; out << "\\u00" << hex[c >> 4] << hex[c & 15]; }
        else out << c;
    }
    out << '"'; return out.str();
}
static char* copy(std::string const& s) {
    auto* p = static_cast<char*>(std::malloc(s.size() + 1));
    if (!p) throw std::bad_alloc();
    std::memcpy(p, s.c_str(), s.size() + 1); return p;
}
extern "C" {
const char* ts_error() noexcept { return last_error.c_str(); }
void ts_free(char* p) noexcept { std::free(p); }
void ts_destroy(Engine* p) noexcept { delete p; }
Engine* ts_create(const char* source, const char* dir, int limit) noexcept {
    try {
        lt::settings_pack settings;
        settings.set_int(lt::settings_pack::alert_mask, static_cast<int>(lt::alert_category::error));
        settings.set_int(lt::settings_pack::download_rate_limit, limit);
        settings.set_str(lt::settings_pack::listen_interfaces, "0.0.0.0:0,[::]:0");
        settings.set_bool(lt::settings_pack::enable_upnp, false);
        settings.set_bool(lt::settings_pack::enable_natpmp, false);
        lt::add_torrent_params params;
        std::string input(source);
        if (input.rfind("magnet:?", 0) == 0) params = lt::parse_magnet_uri(input);
        else params.ti = std::make_shared<lt::torrent_info>(input);
        params.save_path = dir;
        // Metadata exchange works in upload mode; payload cannot start before validation.
        params.flags &= ~(lt::torrent_flags::paused | lt::torrent_flags::auto_managed);
        params.flags |= lt::torrent_flags::upload_mode;
        auto engine = std::make_unique<Engine>(settings);
        // Honor the CLI's explicit bandwidth cap for LAN and loopback peers too.
        lt::ip_filter classes;
        auto mask = 1u << static_cast<unsigned>(lt::session::global_peer_class_id);
        classes.add_rule(lt::make_address("0.0.0.0"), lt::make_address("255.255.255.255"), mask);
        classes.add_rule(lt::make_address("::"), lt::make_address("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"), mask);
        engine->session.set_peer_class_filter(classes);
        engine->handle = engine->session.add_torrent(params);
        return engine.release();
    } catch (std::exception const& e) { last_error = e.what(); return nullptr; }
    catch (...) { last_error = "unknown native exception"; return nullptr; }
}
char* ts_metadata(Engine* engine) noexcept {
    try {
        engine->check();
        auto info = engine->handle.torrent_file();
        if (!info) return copy("null");
        auto const& files = info->files();
        std::ostringstream out;
        out << "{\"piece_length\":" << info->piece_length() << ",\"piece_count\":" << info->num_pieces() << ",\"files\":[";
        bool first = true;
        for (auto index : files.file_range()) {
            if (!first) out << ',';
            first = false;
            auto path = files.file_path(index);
#ifdef _WIN32
            std::replace(path.begin(), path.end(), '\\', '/');
#endif
            out << "{\"index\":" << static_cast<int>(index)
                << ",\"path\":" << quote(path)
                << ",\"size\":" << files.file_size(index)
                << ",\"offset\":" << files.file_offset(index)
                << ",\"symlink\":" << (bool(files.file_flags(index) & lt::file_storage::flag_symlink) ? "true" : "false")
                << ",\"pad\":" << (files.pad_file_at(index) ? "true" : "false") << '}';
        }
        out << "]}"; return copy(out.str());
    } catch (std::exception const& e) { last_error = e.what(); return nullptr; }
    catch (...) { last_error = "unknown native exception"; return nullptr; }
}
int ts_select(Engine* engine, int file) noexcept {
    try {
        engine->check(); auto info = engine->handle.torrent_file();
        if (!info || file < 0 || file >= info->num_files()) throw std::runtime_error("invalid file index");
        auto const& fs = info->files(); auto f = lt::file_index_t(file);
        std::vector<lt::download_priority_t> priorities(info->num_pieces(), lt::dont_download);
        auto start = fs.file_offset(f); auto end = start + fs.file_size(f);
        if (end <= start) throw std::runtime_error("empty file");
        for (auto p = start / info->piece_length(); p <= (end - 1) / info->piece_length(); ++p)
            priorities.at(p) = lt::download_priority_t(1);
        engine->handle.prioritize_pieces(priorities);
        engine->handle.unset_flags(lt::torrent_flags::upload_mode);
        return 0;
    } catch (std::exception const& e) { last_error = e.what(); return -1; }
    catch (...) { last_error = "unknown native exception"; return -1; }
}
struct Update { unsigned piece; unsigned priority; int deadline_ms; };
int ts_priorities(Engine* engine, Update const* updates, size_t len) noexcept {
    try {
        engine->check(); auto info = engine->handle.torrent_file();
        for (size_t i = 0; i < len; ++i) {
            auto const& u = updates[i];
            if (!info || u.piece >= unsigned(info->num_pieces()) || u.priority > 7) throw std::runtime_error("invalid priority update");
            auto p = lt::piece_index_t(u.piece);
            engine->handle.piece_priority(p, lt::download_priority_t(u.priority));
            if (u.deadline_ms < 0) engine->handle.reset_piece_deadline(p);
            else engine->handle.set_piece_deadline(p, u.deadline_ms);
        }
        return 0;
    } catch (std::exception const& e) { last_error = e.what(); return -1; }
    catch (...) { last_error = "unknown native exception"; return -1; }
}
char* ts_status(Engine* engine) noexcept {
    try {
        engine->check(); auto s = engine->handle.status(lt::torrent_handle::query_pieces);
        std::ostringstream out; out << "{\"download_rate\":" << s.download_payload_rate
            << ",\"downloaded\":" << s.total_payload_download << ",\"peers\":" << s.num_peers << ",\"completed\":[";
        for (int p = 0; p < s.pieces.size(); ++p) { if (p) out << ','; out << (s.pieces[lt::piece_index_t(p)] ? "true" : "false"); }
        out << "]}"; return copy(out.str());
    } catch (std::exception const& e) { last_error = e.what(); return nullptr; }
    catch (...) { last_error = "unknown native exception"; return nullptr; }
}
#ifdef TS_INTEGRATION_TESTS
char* ts_magnet(Engine* engine) noexcept {
    try { return copy(lt::make_magnet_uri(engine->handle)); }
    catch (std::exception const& e) { last_error = e.what(); return nullptr; }
    catch (...) { last_error = "unknown native exception"; return nullptr; }
}
int ts_listen_port(Engine* engine) noexcept { return engine->session.listen_port(); }
int ts_connect(Engine* engine, const char* host, unsigned short port) noexcept {
    try { engine->handle.connect_peer(lt::tcp::endpoint(lt::make_address(host), port)); return 0; }
    catch (std::exception const& e) { last_error = e.what(); return -1; }
    catch (...) { last_error = "unknown native exception"; return -1; }
}
char* ts_priority_snapshot(Engine* engine) noexcept {
    try {
        std::ostringstream out; out << '['; bool first = true;
        for (auto p : engine->handle.get_piece_priorities()) { if (!first) out << ','; first = false; out << int(p); }
        out << ']'; return copy(out.str());
    } catch (std::exception const& e) { last_error = e.what(); return nullptr; }
    catch (...) { last_error = "unknown native exception"; return nullptr; }
}
// Generate real SHA1 piece hashes for a local, deterministic test swarm.
int ts_make_fixture(const char* root, const char* name, const char* output) noexcept {
    try {
        lt::file_storage files;
        auto source = std::filesystem::u8path(root) / std::filesystem::u8path(name);
        if (std::filesystem::is_directory(source)) {
            std::vector<std::filesystem::path> entries;
            for (auto const& entry : std::filesystem::directory_iterator(source)) entries.push_back(entry.path());
            std::sort(entries.begin(), entries.end());
            for (auto const& entry : entries) files.add_file(std::string(name) + "/" + entry.filename().u8string(), std::filesystem::file_size(entry));
        } else files.add_file(name, std::filesystem::file_size(source));
        lt::create_torrent creator(files, 256 * 1024, lt::create_torrent::v1_only);
        lt::set_piece_hashes(creator, root);
        std::vector<char> encoded; lt::bencode(std::back_inserter(encoded), creator.generate());
        std::ofstream stream(std::filesystem::u8path(output), std::ios::binary); stream.write(encoded.data(), encoded.size());
        if (!stream) throw std::runtime_error("failed to write fixture");
        return 0;
    } catch (std::exception const& e) { last_error = e.what(); return -1; }
    catch (...) { last_error = "unknown native exception"; return -1; }
}
#endif
}
