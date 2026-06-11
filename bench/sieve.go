package main

import "fmt"

func sieve(limit int) int {
	isPrime := make([]bool, limit+1)
	for i := 2; i <= limit; i++ {
		isPrime[i] = true
	}

	for i := 2; i*i <= limit; i++ {
		if isPrime[i] {
			for j := i * i; j <= limit; j += i {
				isPrime[j] = false
			}
		}
	}

	count := 0
	for i := 2; i <= limit; i++ {
		if isPrime[i] {
			count++
		}
	}
	return count
}

func main() {
	fmt.Println(sieve(50000000))
}
