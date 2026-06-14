package main

import (
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type result struct {
	latencyNs int64
	status    int
	bytes     int64
	err       bool
	errText   string
}

func percentile(sorted []int64, pct float64) int64 {
	if len(sorted) == 0 {
		return 0
	}
	idx := int((pct / 100.0) * float64(len(sorted)-1))
	if idx < 0 {
		idx = 0
	}
	if idx >= len(sorted) {
		idx = len(sorted) - 1
	}
	return sorted[idx]
}

func ms(ns int64) float64 {
	return float64(ns) / float64(time.Millisecond)
}

func runBatch(client *http.Client, baseURL string, paths []string, requests int, concurrency int) ([]result, time.Duration) {
	if requests <= 0 {
		return nil, 0
	}
	if concurrency <= 0 {
		concurrency = 1
	}
	if concurrency > requests {
		concurrency = requests
	}

	results := make([]result, requests)
	var next int64
	var wg sync.WaitGroup
	startWall := time.Now()

	for worker := 0; worker < concurrency; worker++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for {
				idx := int(atomic.AddInt64(&next, 1)) - 1
				if idx >= requests {
					return
				}

				path := paths[idx%len(paths)]
				url := baseURL + path
				req, err := http.NewRequest(http.MethodGet, url, nil)
				if err != nil {
					results[idx] = result{err: true, errText: err.Error()}
					continue
				}
				req.Header.Set("Accept", "*/*")
				req.Header.Set("User-Agent", "bolide-http-load/1")

				start := time.Now()
				resp, err := client.Do(req)
				elapsed := time.Since(start)
				if err != nil {
					results[idx] = result{latencyNs: elapsed.Nanoseconds(), err: true, errText: err.Error()}
					continue
				}
				n, readErr := io.Copy(io.Discard, resp.Body)
				closeErr := resp.Body.Close()
				errText := ""
				if readErr != nil {
					errText = readErr.Error()
				}
				if closeErr != nil {
					if errText != "" {
						errText += "; "
					}
					errText += closeErr.Error()
				}
				results[idx] = result{
					latencyNs: elapsed.Nanoseconds(),
					status:    resp.StatusCode,
					bytes:     n,
					err:       readErr != nil || closeErr != nil,
					errText:   errText,
				}
			}
		}()
	}

	wg.Wait()
	return results, time.Since(startWall)
}

func summarize(results []result, elapsed time.Duration, label string, concurrency int) {
	latencies := make([]int64, 0, len(results))
	var ok, non2xx, errors int
	var totalBytes int64
	var totalLatency int64
	var maxLatency int64
	errorCounts := make(map[string]int)

	for _, r := range results {
		if r.err {
			errors++
			text := r.errText
			if text == "" {
				text = "unknown"
			}
			errorCounts[text]++
		}
		if r.status >= 200 && r.status < 400 && !r.err {
			ok++
		} else if !r.err {
			non2xx++
		}
		if r.latencyNs > 0 {
			latencies = append(latencies, r.latencyNs)
			totalLatency += r.latencyNs
			if r.latencyNs > maxLatency {
				maxLatency = r.latencyNs
			}
		}
		totalBytes += r.bytes
	}

	sort.Slice(latencies, func(i, j int) bool { return latencies[i] < latencies[j] })

	avg := 0.0
	if len(latencies) > 0 {
		avg = ms(totalLatency / int64(len(latencies)))
	}

	seconds := elapsed.Seconds()
	rps := 0.0
	mbps := 0.0
	if seconds > 0 {
		rps = float64(len(results)) / seconds
		mbps = (float64(totalBytes) / 1024.0 / 1024.0) / seconds
	}

	fmt.Printf("label:        %s\n", label)
	fmt.Printf("requests:     %d\n", len(results))
	fmt.Printf("concurrency:  %d\n", concurrency)
	fmt.Printf("elapsed:      %.3fs\n", seconds)
	fmt.Printf("rps:          %.0f\n", rps)
	fmt.Printf("throughput:   %.2f MB/s\n", mbps)
	fmt.Printf("ok:           %d\n", ok)
	fmt.Printf("non2xx:       %d\n", non2xx)
	fmt.Printf("errors:       %d\n", errors)
	fmt.Printf("lat_avg:      %.3f ms\n", avg)
	fmt.Printf("lat_p50:      %.3f ms\n", ms(percentile(latencies, 50)))
	fmt.Printf("lat_p90:      %.3f ms\n", ms(percentile(latencies, 90)))
	fmt.Printf("lat_p99:      %.3f ms\n", ms(percentile(latencies, 99)))
	fmt.Printf("lat_max:      %.3f ms\n", ms(maxLatency))
	if len(errorCounts) > 0 {
		type errorItem struct {
			text  string
			count int
		}
		items := make([]errorItem, 0, len(errorCounts))
		for text, count := range errorCounts {
			items = append(items, errorItem{text: text, count: count})
		}
		sort.Slice(items, func(i, j int) bool {
			if items[i].count == items[j].count {
				return items[i].text < items[j].text
			}
			return items[i].count > items[j].count
		})
		limit := len(items)
		if limit > 5 {
			limit = 5
		}
		for i := 0; i < limit; i++ {
			fmt.Printf("error_%d:      %d x %s\n", i+1, items[i].count, items[i].text)
		}
	}
}

func parsePaths(value string) []string {
	parts := strings.Split(value, ",")
	paths := make([]string, 0, len(parts))
	for _, part := range parts {
		path := strings.TrimSpace(part)
		if path == "" {
			continue
		}
		if !strings.HasPrefix(path, "/") {
			path = "/" + path
		}
		paths = append(paths, path)
	}
	if len(paths) == 0 {
		return []string{"/"}
	}
	return paths
}

func main() {
	baseURL := flag.String("url", "http://127.0.0.1:8000", "base URL, without trailing slash")
	pathList := flag.String("paths", "/", "comma-separated request paths")
	requests := flag.Int("n", 100000, "measured requests")
	concurrency := flag.Int("c", 128, "concurrent workers")
	warmup := flag.Int("warmup", 5000, "warm-up requests before measurement")
	timeout := flag.Duration("timeout", 10*time.Second, "per-request timeout")
	noKeepAlive := flag.Bool("no-keepalive", false, "disable HTTP keep-alive")
	label := flag.String("label", "http", "result label")
	flag.Parse()

	base := strings.TrimRight(*baseURL, "/")
	paths := parsePaths(*pathList)
	maxConns := *concurrency
	if maxConns < 1 {
		maxConns = 1
	}
	dialer := &net.Dialer{
		Timeout:   *timeout,
		KeepAlive: 30 * time.Second,
	}
	transport := &http.Transport{
		DisableCompression:  true,
		DisableKeepAlives:   *noKeepAlive,
		DialContext:         dialer.DialContext,
		MaxIdleConns:        maxConns,
		MaxIdleConnsPerHost: maxConns,
		MaxConnsPerHost:     maxConns,
		IdleConnTimeout:     90 * time.Second,
	}
	client := &http.Client{
		Transport: transport,
		Timeout:   *timeout,
	}
	defer transport.CloseIdleConnections()

	if *warmup > 0 {
		_, _ = runBatch(client, base, paths, *warmup, *concurrency)
	}

	results, elapsed := runBatch(client, base, paths, *requests, *concurrency)
	summarize(results, elapsed, *label, *concurrency)
}
