#include <arpa/inet.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#include <atomic>
#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <cerrno>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iomanip>
#include <iostream>
#include <map>
#include <mutex>
#include <signal.h>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include "sherpa-onnx/c-api/c-api.h"
#include "text_visibility.h"

namespace {

constexpr size_t kMaxHeaderBytes = 32 * 1024;
constexpr size_t kMaxAudioBytes = 128 * 1024 * 1024;
constexpr size_t kMaxResponseBytes = 16 * 1024 * 1024;
constexpr size_t kMaxSegments = 4096;
constexpr int32_t kMaxDecodedTokens = 65536;
constexpr size_t kMaxConnections = 32;
constexpr int kSocketIoTimeoutSeconds = 30;
constexpr auto kHeaderDeadline = std::chrono::seconds(5);
constexpr auto kBodyDeadline = std::chrono::seconds(30);
constexpr auto kInferenceDeadline = std::chrono::seconds(270);

struct Options {
  int listener_fd = -1;
  int capability_fd = -1;
  const char *catalog_id = nullptr;
  const char *model = nullptr;
  const char *tokens = nullptr;
  const char *vad_model = nullptr;
  int32_t num_threads = 1;
};

struct Wav {
  int32_t sample_rate = 0;
  std::vector<float> samples;
};

struct Server {
  Options options;
  std::string capability;
  const SherpaOnnxOfflineRecognizer *recognizer = nullptr;
  const SherpaOnnxVoiceActivityDetector *vad = nullptr;
  std::atomic<bool> inference_busy{false};
  std::mutex connections_mutex;
  std::condition_variable connections_changed;
  size_t active_connections = 0;
};

struct HttpRequest {
  std::string method;
  std::string target;
  std::map<std::string, std::string> headers;
  std::vector<uint8_t> body;
};

void Usage(const char *program) {
  std::cerr << "Usage: " << program
            << " --listener-fd N --capability-fd N --catalog-id ID"
               " --model FILE --tokens FILE --vad-model FILE"
               " [--num-threads N]\n";
}

bool ParseInt(const char *value, int *result) {
  char *end = nullptr;
  errno = 0;
  long parsed = std::strtol(value, &end, 10);
  if (errno || end == value || *end || parsed < 0 || parsed > INT32_MAX) return false;
  *result = static_cast<int>(parsed);
  return true;
}

bool ParseOptions(int argc, char **argv, Options *options) {
  for (int i = 1; i < argc; ++i) {
    auto take = [&](const char **out) {
      if (i + 1 >= argc) return false;
      *out = argv[++i];
      return true;
    };
    const char *value = nullptr;
    if (std::strcmp(argv[i], "--listener-fd") == 0 && take(&value)) {
      if (!ParseInt(value, &options->listener_fd)) return false;
    } else if (std::strcmp(argv[i], "--capability-fd") == 0 && take(&value)) {
      if (!ParseInt(value, &options->capability_fd)) return false;
    } else if (std::strcmp(argv[i], "--catalog-id") == 0 && take(&options->catalog_id)) {
    } else if (std::strcmp(argv[i], "--model") == 0 && take(&options->model)) {
    } else if (std::strcmp(argv[i], "--tokens") == 0 && take(&options->tokens)) {
    } else if (std::strcmp(argv[i], "--vad-model") == 0 && take(&options->vad_model)) {
    } else if (std::strcmp(argv[i], "--num-threads") == 0 && take(&value)) {
      int parsed = 0;
      if (!ParseInt(value, &parsed) || parsed < 1) return false;
      options->num_threads = parsed;
    } else {
      return false;
    }
  }
  return options->listener_fd >= 0 && options->capability_fd >= 0 &&
         options->catalog_id && options->model && options->tokens && options->vad_model;
}

bool IsLowerHexCapability(const std::string &value) {
  if (value.size() != 64) return false;
  for (unsigned char c : value) {
    if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return false;
  }
  return true;
}

bool ReadCapability(int fd, std::string *value) {
  char bytes[64];
  size_t offset = 0;
  while (offset < sizeof(bytes)) {
    ssize_t n = read(fd, bytes + offset, sizeof(bytes) - offset);
    if (n <= 0) return false;
    offset += static_cast<size_t>(n);
  }
  close(fd);
  value->assign(bytes, sizeof(bytes));
  return IsLowerHexCapability(*value);
}

bool ConstantTimeEqual(const std::string &left, const std::string &right) {
  size_t size = left.size() > right.size() ? left.size() : right.size();
  size_t difference = left.size() ^ right.size();
  for (size_t i = 0; i < size; ++i) {
    unsigned char a = i < left.size() ? left[i] : 0;
    unsigned char b = i < right.size() ? right[i] : 0;
    difference |= a ^ b;
  }
  return difference == 0;
}

int ValidateInheritedListener(int fd) {
  sockaddr_in address{};
  socklen_t size = sizeof(address);
  int socket_type = 0;
  socklen_t socket_type_size = sizeof(socket_type);
  if (getsockname(fd, reinterpret_cast<sockaddr *>(&address), &size) != 0) return 1;
  if (address.sin_family != AF_INET) return 2;
  if (ntohl(address.sin_addr.s_addr) != INADDR_LOOPBACK) return 3;
  if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &socket_type, &socket_type_size) != 0) return 4;
  if (socket_type != SOCK_STREAM) return 5;
  return 0;
}

uint16_t ReadLe16(const uint8_t *p) {
  return static_cast<uint16_t>(p[0]) | static_cast<uint16_t>(p[1]) << 8;
}

uint32_t ReadLe32(const uint8_t *p) {
  return static_cast<uint32_t>(p[0]) | static_cast<uint32_t>(p[1]) << 8 |
         static_cast<uint32_t>(p[2]) << 16 | static_cast<uint32_t>(p[3]) << 24;
}

bool ParsePcm16MonoWav(const std::vector<uint8_t> &bytes, Wav *wav) {
  if (bytes.size() < 44 || std::memcmp(bytes.data(), "RIFF", 4) ||
      std::memcmp(bytes.data() + 8, "WAVE", 4)) return false;
  bool found_format = false;
  bool found_data = false;
  uint16_t format = 0, channels = 0, bits = 0;
  uint32_t sample_rate = 0;
  const uint8_t *pcm = nullptr;
  size_t pcm_size = 0;
  for (size_t offset = 12; offset + 8 <= bytes.size();) {
    const uint8_t *chunk = bytes.data() + offset;
    uint32_t size = ReadLe32(chunk + 4);
    size_t payload = offset + 8;
    if (payload + size > bytes.size()) return false;
    if (!std::memcmp(chunk, "fmt ", 4) && size >= 16) {
      format = ReadLe16(bytes.data() + payload);
      channels = ReadLe16(bytes.data() + payload + 2);
      sample_rate = ReadLe32(bytes.data() + payload + 4);
      bits = ReadLe16(bytes.data() + payload + 14);
      found_format = true;
    } else if (!std::memcmp(chunk, "data", 4)) {
      pcm = bytes.data() + payload;
      pcm_size = size;
      found_data = true;
    }
    offset = payload + size + (size & 1U);
  }
  if (!found_format || !found_data || format != 1 || channels != 1 ||
      sample_rate != 16000 || bits != 16 || pcm_size == 0 || (pcm_size & 1U)) return false;
  wav->sample_rate = sample_rate;
  wav->samples.resize(pcm_size / 2);
  for (size_t i = 0; i < wav->samples.size(); ++i) {
    int16_t sample = static_cast<int16_t>(ReadLe16(pcm + i * 2));
    wav->samples[i] = static_cast<float>(sample) / 32768.0F;
  }
  return true;
}

std::string JsonString(const char *text) {
  if (!text) return "null";
  std::ostringstream out;
  out << '"';
  for (const unsigned char *p = reinterpret_cast<const unsigned char *>(text); *p; ++p) {
    switch (*p) {
      case '"': out << "\\\""; break;
      case '\\': out << "\\\\"; break;
      case '\b': out << "\\b"; break;
      case '\f': out << "\\f"; break;
      case '\n': out << "\\n"; break;
      case '\r': out << "\\r"; break;
      case '\t': out << "\\t"; break;
      default:
        if (*p < 0x20) {
          out << "\\u" << std::hex << std::setw(4) << std::setfill('0') << static_cast<int>(*p)
              << std::dec;
        } else {
          out << static_cast<char>(*p);
        }
    }
  }
  out << '"';
  return out.str();
}

std::string Lower(std::string value) {
  for (char &c : value) {
    if (c >= 'A' && c <= 'Z') c = static_cast<char>(c - 'A' + 'a');
  }
  return value;
}

bool WaitReadable(int fd, std::chrono::steady_clock::time_point deadline) {
  for (;;) {
    auto remaining = deadline - std::chrono::steady_clock::now();
    if (remaining <= std::chrono::steady_clock::duration::zero()) return false;
    auto milliseconds = std::chrono::duration_cast<std::chrono::milliseconds>(remaining).count();
    pollfd descriptor{fd, POLLIN, 0};
    int ready = poll(&descriptor, 1, static_cast<int>(std::max<int64_t>(1, milliseconds)));
    if (ready > 0) return (descriptor.revents & (POLLIN | POLLHUP)) != 0;
    if (ready == 0) return false;
    if (errno != EINTR) return false;
  }
}

bool ReadExact(int fd, uint8_t *buffer, size_t size,
               std::chrono::steady_clock::time_point deadline) {
  size_t offset = 0;
  while (offset < size) {
    if (!WaitReadable(fd, deadline)) return false;
    ssize_t n = recv(fd, buffer + offset, size - offset, 0);
    if (n <= 0) return false;
    offset += static_cast<size_t>(n);
  }
  return true;
}

bool ParseRequestHeaders(int fd, HttpRequest *request, size_t *content_length,
                         std::string *error_code) {
  auto deadline = std::chrono::steady_clock::now() + kHeaderDeadline;
  std::string header;
  char byte;
  while (header.size() < kMaxHeaderBytes) {
    if (!WaitReadable(fd, deadline)) return false;
    ssize_t n = recv(fd, &byte, 1, 0);
    if (n <= 0) return false;
    header.push_back(byte);
    if (header.size() >= 4 && header.compare(header.size() - 4, 4, "\r\n\r\n") == 0) break;
  }
  if (header.size() >= kMaxHeaderBytes || header.compare(header.size() - 4, 4, "\r\n\r\n")) {
    *error_code = "invalid_request";
    return false;
  }
  std::istringstream input(header);
  std::string request_line;
  if (!std::getline(input, request_line)) {
    *error_code = "invalid_request";
    return false;
  }
  if (!request_line.empty() && request_line.back() == '\r') request_line.pop_back();
  std::istringstream first(request_line);
  std::string version, extra;
  if (!(first >> request->method >> request->target >> version) || first >> extra || version != "HTTP/1.1" ||
      request->target.empty() || request->target[0] != '/') {
    *error_code = "invalid_request";
    return false;
  }
  std::string line;
  while (std::getline(input, line)) {
    if (!line.empty() && line.back() == '\r') line.pop_back();
    if (line.empty()) break;
    size_t colon = line.find(':');
    if (colon == std::string::npos) {
      *error_code = "invalid_request";
      return false;
    }
    std::string key = Lower(line.substr(0, colon));
    size_t start = colon + 1;
    while (start < line.size() && (line[start] == ' ' || line[start] == '\t')) ++start;
    if (!request->headers.emplace(key, line.substr(start)).second) {
      *error_code = "invalid_request";
      return false;
    }
  }
  if (request->headers.count("transfer-encoding") || request->headers.count("expect")) {
    *error_code = "invalid_request";
    return false;
  }
  *content_length = 0;
  auto length = request->headers.find("content-length");
  if (length != request->headers.end()) {
    char *end = nullptr;
    errno = 0;
    unsigned long long parsed = std::strtoull(length->second.c_str(), &end, 10);
    if (errno || end == length->second.c_str() || *end || parsed > kMaxAudioBytes) {
      *error_code = parsed > kMaxAudioBytes ? "request_too_large" : "invalid_request";
      return false;
    }
    *content_length = static_cast<size_t>(parsed);
  }
  return true;
}

bool ReadRequestBody(int fd, size_t content_length, HttpRequest *request) {
  request->body.resize(content_length);
  return content_length == 0 ||
         ReadExact(fd, request->body.data(), content_length,
                   std::chrono::steady_clock::now() + kBodyDeadline);
}

const char *Reason(int status) {
  switch (status) {
    case 200: return "OK";
    case 400: return "Bad Request";
    case 401: return "Unauthorized";
    case 404: return "Not Found";
    case 405: return "Method Not Allowed";
    case 409: return "Conflict";
    case 413: return "Payload Too Large";
    case 415: return "Unsupported Media Type";
    case 422: return "Unprocessable Content";
    case 500: return "Internal Server Error";
    case 503: return "Service Unavailable";
    case 504: return "Gateway Timeout";
    default: return "Error";
  }
}

void SendResponse(int fd, int status, const std::string &body) {
  std::ostringstream response;
  response << "HTTP/1.1 " << status << ' ' << Reason(status)
           << "\r\nContent-Type: application/json\r\nContent-Length: " << body.size()
           << "\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n" << body;
  std::string bytes = response.str();
  size_t offset = 0;
  while (offset < bytes.size()) {
    ssize_t n = send(fd, bytes.data() + offset, bytes.size() - offset, 0);
    if (n <= 0) break;
    offset += static_cast<size_t>(n);
  }
}

void SendError(int fd, int status, const char *code, bool retryable, const char *message) {
  std::ostringstream body;
  body << "{\"protocol_version\":1,\"error\":{\"code\":" << JsonString(code)
       << ",\"retryable\":" << (retryable ? "true" : "false")
       << ",\"message\":" << JsonString(message) << "}}";
  SendResponse(fd, status, body.str());
}

bool Decode(Server *server, const Wav &wav, std::string *json) {
  SherpaOnnxVoiceActivityDetectorReset(server->vad);
  for (size_t offset = 0; offset < wav.samples.size(); offset += 512) {
    int32_t count = static_cast<int32_t>(std::min<size_t>(512, wav.samples.size() - offset));
    SherpaOnnxVoiceActivityDetectorAcceptWaveform(server->vad, wav.samples.data() + offset, count);
  }
  SherpaOnnxVoiceActivityDetectorFlush(server->vad);
  std::vector<std::string> segments;
  std::string full_text;
  int64_t decoded_token_count = 0;
  int64_t skipped_empty_segments = 0;
  bool post_decode_text_modified = false;
  while (!SherpaOnnxVoiceActivityDetectorEmpty(server->vad)) {
    const SherpaOnnxSpeechSegment *speech = SherpaOnnxVoiceActivityDetectorFront(server->vad);
    if (!speech || !speech->samples || speech->start < 0 || speech->n <= 0 ||
        segments.size() >= kMaxSegments) {
      if (speech) SherpaOnnxDestroySpeechSegment(speech);
      SherpaOnnxVoiceActivityDetectorPop(server->vad);
      return false;
    }
    const SherpaOnnxOfflineStream *stream = SherpaOnnxCreateOfflineStream(server->recognizer);
    if (!stream) {
      SherpaOnnxDestroySpeechSegment(speech);
      SherpaOnnxVoiceActivityDetectorPop(server->vad);
      return false;
    }
    SherpaOnnxAcceptWaveformOffline(stream, wav.sample_rate, speech->samples, speech->n);
    SherpaOnnxDecodeOfflineStream(server->recognizer, stream);
    const SherpaOnnxOfflineRecognizerResult *result = SherpaOnnxGetOfflineStreamResult(stream);
    if (!result) {
      SherpaOnnxDestroyOfflineStream(stream);
      SherpaOnnxDestroySpeechSegment(speech);
      SherpaOnnxVoiceActivityDetectorPop(server->vad);
      return false;
    }
    // A decoded segment without any visible text is skipped instead of failing
    // the whole request; if every segment is skipped the response stays the
    // legal empty variant (text="" and segments=[]).
    if (!seasnail_sidecar::HasVisibleText(result->text) || result->count <= 0) {
      ++skipped_empty_segments;
      SherpaOnnxDestroyOfflineRecognizerResult(result);
      SherpaOnnxDestroyOfflineStream(stream);
      SherpaOnnxDestroySpeechSegment(speech);
      SherpaOnnxVoiceActivityDetectorPop(server->vad);
      continue;
    }
    bool invalid_result = result->count > kMaxDecodedTokens || !result->tokens_arr;
    if (!invalid_result) {
      decoded_token_count += result->count;
      invalid_result = decoded_token_count > kMaxDecodedTokens;
    }
    for (int32_t i = 0; !invalid_result && i < result->count; ++i)
      invalid_result = !result->tokens_arr[i] || !*result->tokens_arr[i];
    if (invalid_result) {
      SherpaOnnxDestroyOfflineRecognizerResult(result);
      SherpaOnnxDestroyOfflineStream(stream);
      SherpaOnnxDestroySpeechSegment(speech);
      SherpaOnnxVoiceActivityDetectorPop(server->vad);
      return false;
    }
    double start = static_cast<double>(speech->start) / wav.sample_rate;
    double end = static_cast<double>(speech->start + speech->n) / wav.sample_rate;
    bool timeline = result->timestamps != nullptr;
    double previous = -1.0;
    for (int32_t i = 0; timeline && i < result->count; ++i) {
      double timestamp = start + result->timestamps[i];
      if (!std::isfinite(timestamp) || timestamp < start || timestamp > end || timestamp < previous)
        timeline = false;
      previous = timestamp;
    }
    std::ostringstream segment;
    std::string decoded_text;
    segment << std::fixed << std::setprecision(6)
            << "{\"start_seconds\":" << start << ",\"end_seconds\":" << end
            << ",\"text\":" << JsonString(result->text) << ",\"decoded_tokens\":[";
    for (int32_t i = 0; i < result->count; ++i) {
      if (i) segment << ',';
      segment << JsonString(result->tokens_arr[i]);
      decoded_text += result->tokens_arr[i];
    }
    segment << "],\"token_start_seconds\":";
    if (timeline) {
      segment << '[';
      for (int32_t i = 0; i < result->count; ++i) {
        if (i) segment << ',';
        segment << start + result->timestamps[i];
      }
      segment << ']';
    } else {
      segment << "null";
    }
    segment << ",\"language\":" << JsonString(result->lang)
            << ",\"event\":" << JsonString(result->event) << '}';
    full_text += result->text;
    post_decode_text_modified = post_decode_text_modified || decoded_text != result->text;
    segments.push_back(segment.str());
    SherpaOnnxDestroyOfflineRecognizerResult(result);
    SherpaOnnxDestroyOfflineStream(stream);
    SherpaOnnxDestroySpeechSegment(speech);
    SherpaOnnxVoiceActivityDetectorPop(server->vad);
  }
  if (skipped_empty_segments > 0) {
    std::cerr << "sidecar_notice=empty_segments_skipped count=" << skipped_empty_segments << "\n";
  }
  std::ostringstream response;
  response << "{\"protocol_version\":1,\"catalog_id\":" << JsonString(server->options.catalog_id)
           << ",\"text\":" << JsonString(full_text.c_str()) << ",\"segments\":[";
  for (size_t i = 0; i < segments.size(); ++i) {
    if (i) response << ',';
    response << segments[i];
  }
  response << "],\"transforms\":{\"use_itn\":true,\"rule_fsts\":false,"
              "\"homophone_replacer\":false,\"post_decode_text_modified\":"
           << (post_decode_text_modified ? "true" : "false") << "}}";
  *json = response.str();
  return json->size() <= kMaxResponseBytes;
}

void HandleConnection(Server *server, int fd) {
  timeval timeout{kSocketIoTimeoutSeconds, 0};
  setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
  setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
  sockaddr_in peer{};
  socklen_t peer_size = sizeof(peer);
  if (getpeername(fd, reinterpret_cast<sockaddr *>(&peer), &peer_size) ||
      peer.sin_family != AF_INET || ntohl(peer.sin_addr.s_addr) != INADDR_LOOPBACK) {
    close(fd);
    return;
  }
  HttpRequest request;
  size_t content_length = 0;
  std::string parse_error;
  if (!ParseRequestHeaders(fd, &request, &content_length, &parse_error)) {
    if (parse_error == "request_too_large")
      SendError(fd, 413, "request_too_large", false, "audio request exceeds the configured limit");
    else
      SendError(fd, 400, "invalid_request", false, "malformed HTTP request");
    close(fd);
    return;
  }
  auto capability = request.headers.find("x-seasnail-capability");
  if (capability == request.headers.end() || !ConstantTimeEqual(capability->second, server->capability)) {
    SendError(fd, 401, "unauthorized", false, "request capability rejected");
    close(fd);
    return;
  }
  auto version = request.headers.find("x-seasnail-protocol-version");
  if (version == request.headers.end() || version->second != "1") {
    SendError(fd, 400, "protocol_mismatch", false, "unsupported sidecar protocol version");
    close(fd);
    return;
  }
  if (request.target == "/health") {
    if (request.method != "GET" || content_length != 0) {
      SendError(fd, 405, "method_not_allowed", false, "health requires GET with an empty body");
    } else {
      std::ostringstream body;
      body << "{\"protocol_version\":1,\"status\":\"ready\",\"catalog_id\":"
           << JsonString(server->options.catalog_id) << '}';
      SendResponse(fd, 200, body.str());
    }
    close(fd);
    return;
  }
  if (request.target != "/v1/transcribe") {
    SendError(fd, 404, "not_found", false, "unknown sidecar endpoint");
    close(fd);
    return;
  }
  if (request.method != "POST") {
    SendError(fd, 405, "method_not_allowed", false, "transcription requires POST");
    close(fd);
    return;
  }
  auto content_type = request.headers.find("content-type");
  if (content_type == request.headers.end() || content_type->second != "audio/wav") {
    SendError(fd, 415, "unsupported_media_type", false, "transcription requires audio/wav");
    close(fd);
    return;
  }
  if (request.headers.find("content-length") == request.headers.end()) {
    SendError(fd, 400, "invalid_request", false, "transcription requires a non-empty Content-Length");
    close(fd);
    return;
  }
  if (content_length == 0) {
    SendError(fd, 400, "invalid_request", false, "transcription requires a non-empty Content-Length");
    close(fd);
    return;
  }
  if (!ReadRequestBody(fd, content_length, &request)) {
    SendError(fd, 400, "invalid_request", false, "audio request body was incomplete");
    close(fd);
    return;
  }
  if (server->inference_busy.exchange(true)) {
    SendError(fd, 409, "busy", true, "one transcription is already in progress");
    close(fd);
    return;
  }
  Wav wav;
  if (!ParsePcm16MonoWav(request.body, &wav)) {
    SendError(fd, 422, "invalid_audio", false, "expected mono 16 kHz PCM16 RIFF/WAVE audio");
  } else {
    struct Deadline {
      std::mutex mutex;
      std::condition_variable changed;
      bool finished = false;
    } deadline;
    std::thread watchdog([&deadline, fd] {
      std::unique_lock<std::mutex> lock(deadline.mutex);
      if (!deadline.changed.wait_for(lock, kInferenceDeadline, [&deadline] { return deadline.finished; })) {
        lock.unlock();
        SendError(fd, 504, "inference_timeout", true, "local inference exceeded its deadline");
        shutdown(fd, SHUT_RDWR);
        _Exit(124);
      }
    });
    std::string response;
    bool decoded = Decode(server, wav, &response);
    {
      std::lock_guard<std::mutex> lock(deadline.mutex);
      deadline.finished = true;
    }
    deadline.changed.notify_one();
    watchdog.join();
    if (decoded)
      SendResponse(fd, 200, response);
    else
      SendError(fd, 500, "inference_failed", true, "local inference failed");
  }
  server->inference_busy.store(false);
  close(fd);
}

void HandleConnectionTracked(Server *server, int fd) {
  HandleConnection(server, fd);
  {
    std::lock_guard<std::mutex> lock(server->connections_mutex);
    --server->active_connections;
  }
  server->connections_changed.notify_one();
}

bool Initialize(Server *server) {
  SherpaOnnxOfflineRecognizerConfig recognizer{};
  recognizer.decoding_method = "greedy_search";
  recognizer.model_config.num_threads = server->options.num_threads;
  recognizer.model_config.provider = "cpu";
  recognizer.model_config.tokens = server->options.tokens;
  recognizer.model_config.sense_voice.model = server->options.model;
  recognizer.model_config.sense_voice.language = "auto";
  recognizer.model_config.sense_voice.use_itn = 1;
  server->recognizer = SherpaOnnxCreateOfflineRecognizer(&recognizer);
  if (!server->recognizer) return false;
  SherpaOnnxVadModelConfig vad{};
  vad.silero_vad.model = server->options.vad_model;
  vad.silero_vad.threshold = 0.5F;
  vad.silero_vad.min_silence_duration = 0.5F;
  vad.silero_vad.min_speech_duration = 0.25F;
  vad.silero_vad.max_speech_duration = 20.0F;
  vad.silero_vad.window_size = 512;
  vad.sample_rate = 16000;
  vad.num_threads = server->options.num_threads;
  vad.provider = "cpu";
  server->vad = SherpaOnnxCreateVoiceActivityDetector(&vad, 120.0F);
  return server->vad != nullptr;
}

}  // namespace

int main(int argc, char **argv) {
  signal(SIGPIPE, SIG_IGN);
  Server server;
  if (!ParseOptions(argc, argv, &server.options)) {
    Usage(argv[0]);
    return 2;
  }
  int listener_error = ValidateInheritedListener(server.options.listener_fd);
  if (listener_error != 0) {
    std::cerr << "sidecar_error=invalid_inherited_listener_" << listener_error << "\n";
    return 1;
  }
  if (!ReadCapability(server.options.capability_fd, &server.capability)) {
    std::cerr << "sidecar_error=invalid_capability_channel\n";
    return 1;
  }
  if (!Initialize(&server)) {
    std::cerr << "sidecar_error=model_initialization_failed\n";
    if (server.recognizer) SherpaOnnxDestroyOfflineRecognizer(server.recognizer);
    return 1;
  }
  for (;;) {
    int connection = accept(server.options.listener_fd, nullptr, nullptr);
    if (connection < 0) {
      if (errno == EINTR) continue;
      break;
    }
    {
      std::lock_guard<std::mutex> lock(server.connections_mutex);
      if (server.active_connections >= kMaxConnections) {
        close(connection);
        continue;
      }
      ++server.active_connections;
    }
    try {
      std::thread(HandleConnectionTracked, &server, connection).detach();
    } catch (...) {
      close(connection);
      std::lock_guard<std::mutex> lock(server.connections_mutex);
      --server.active_connections;
      server.connections_changed.notify_one();
    }
  }
  std::unique_lock<std::mutex> connections_lock(server.connections_mutex);
  server.connections_changed.wait(connections_lock, [&server] { return server.active_connections == 0; });
  SherpaOnnxDestroyVoiceActivityDetector(server.vad);
  SherpaOnnxDestroyOfflineRecognizer(server.recognizer);
  return 1;
}
