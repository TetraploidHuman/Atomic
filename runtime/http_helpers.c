// HTTP helper functions for Atomic language
// Uses libcurl to perform HTTP requests
#include <curl/curl.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

// Response buffer for accumulating HTTP response body
struct response_buffer {
    char *data;
    size_t size;
};

// libcurl write callback: appends received data to response_buffer
static size_t write_callback(void *contents, size_t size, size_t nmemb, void *userp) {
    size_t realsize = size * nmemb;
    struct response_buffer *buf = (struct response_buffer *)userp;
    char *ptr = (char *)realloc(buf->data, buf->size + realsize + 1);
    if (!ptr) return 0;
    buf->data = ptr;
    memcpy(&(buf->data[buf->size]), contents, realsize);
    buf->size += realsize;
    buf->data[buf->size] = 0;
    return realsize;
}

// Parse a header line "Name: Value" and add to curl slist
// Returns updated slist, or NULL on failure
static struct curl_slist* add_header_to_slist(struct curl_slist *headers, const char *line) {
    if (!line || !*line) return headers;
    return curl_slist_append(headers, line);
}

/*
 * Perform an HTTP request.
 *
 * Parameters:
 *   method    - HTTP method: "GET", "POST", "PUT", "DELETE", "PATCH"
 *   url       - Full URL including https://
 *   headers   - Headers as "Name: Value\n" separated lines, null-terminated
 *   body      - Request body (NULL for GET/HEAD)
 *   body_len  - Length of body in bytes (0 if no body)
 *
 * Returns a malloc'd string in format: "STATUS_CODE\nRESPONSE_BODY"
 * The caller is responsible for freeing the returned string.
 * On error, returns "0\nError message"
 */
char* atomic_http_request(
    const char* method,
    const char* url,
    const char* headers,
    const char* body,
    int body_len
) {
    CURL *curl = curl_easy_init();
    if (!curl) {
        char *err = (char *)malloc(256);
        snprintf(err, 256, "0\ncurl_easy_init() failed");
        return err;
    }

    struct response_buffer buf = {NULL, 0};
    struct curl_slist *header_list = NULL;
    char *result = NULL;

    // Set URL
    curl_easy_setopt(curl, CURLOPT_URL, url);

    // Set HTTP method
    if (strcmp(method, "POST") == 0) {
        curl_easy_setopt(curl, CURLOPT_POST, 1L);
    } else if (strcmp(method, "PUT") == 0) {
        curl_easy_setopt(curl, CURLOPT_CUSTOMREQUEST, "PUT");
    } else if (strcmp(method, "DELETE") == 0) {
        curl_easy_setopt(curl, CURLOPT_CUSTOMREQUEST, "DELETE");
    } else if (strcmp(method, "PATCH") == 0) {
        curl_easy_setopt(curl, CURLOPT_CUSTOMREQUEST, "PATCH");
    } else if (strcmp(method, "HEAD") == 0) {
        curl_easy_setopt(curl, CURLOPT_NOBODY, 1L);
    }
    // Default is GET

    // Set request body for POST/PUT/PATCH
    if (body && body_len > 0) {
        curl_easy_setopt(curl, CURLOPT_POSTFIELDS, body);
        curl_easy_setopt(curl, CURLOPT_POSTFIELDSIZE, (long)body_len);
    }

    // Parse and set headers
    if (headers && *headers) {
        // Make a mutable copy of headers for strtok
        char *headers_copy = strdup(headers);
        if (headers_copy) {
            char *line = strtok(headers_copy, "\n");
            while (line) {
                // Skip leading whitespace
                while (*line == ' ' || *line == '\r') line++;
                // Skip empty lines
                if (*line) {
                    header_list = add_header_to_slist(header_list, line);
                }
                line = strtok(NULL, "\n");
            }
            free(headers_copy);
        }
    }

    if (header_list) {
        curl_easy_setopt(curl, CURLOPT_HTTPHEADER, header_list);
    }

    // Set write callback
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, write_callback);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &buf);

    // Enable automatic decompression
    curl_easy_setopt(curl, CURLOPT_ACCEPT_ENCODING, "");

    // Set timeout
    curl_easy_setopt(curl, CURLOPT_TIMEOUT, 120L);

    // Follow redirects
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);

    // Perform the request
    CURLcode res = curl_easy_perform(curl);

    // Get HTTP status code
    long http_code = 0;
    if (res == CURLE_OK) {
        curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &http_code);
    } else {
        http_code = 0;
    }

    // Build result: "STATUS_CODE\nBODY"
    size_t result_size;
    if (res == CURLE_OK) {
        if (buf.data) {
            result_size = 32 + buf.size + 2;
            result = (char *)malloc(result_size);
            snprintf(result, result_size, "%ld\n%s", http_code, buf.data);
        } else {
            result_size = 32 + 3;
            result = (char *)malloc(result_size);
            snprintf(result, result_size, "%ld\n", http_code);
        }
    } else {
        const char *err_msg = curl_easy_strerror(res);
        result_size = 32 + strlen(err_msg) + 3;
        result = (char *)malloc(result_size);
        snprintf(result, result_size, "%ld\ncurl error: %s", http_code, err_msg);
    }

    // Cleanup
    free(buf.data);
    if (header_list) curl_slist_free_all(header_list);
    curl_easy_cleanup(curl);

    return result;
}

// Free a string returned by atomic_http_request
void atomic_http_free(char *ptr) {
    free(ptr);
}
