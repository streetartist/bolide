func sieve(_ limit: Int) -> Int {
    var isPrime = [Bool](repeating: true, count: limit + 1)
    isPrime[0] = false; isPrime[1] = false
    var i = 2
    while i * i <= limit {
        if isPrime[i] {
            var j = i * i
            while j <= limit { isPrime[j] = false; j += i }
        }
        i += 1
    }
    var count = 0
    for i in 2...limit { if isPrime[i] { count += 1 } }
    return count
}
print(sieve(50_000_000))
