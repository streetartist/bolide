def sieve(limit):
    is_prime = bytearray(b'\x01') * (limit + 1)
    is_prime[0] = is_prime[1] = 0
    i = 2
    while i * i <= limit:
        if is_prime[i]:
            j = i * i
            while j <= limit:
                is_prime[j] = 0
                j += i
        i += 1
    count = 0
    i = 2
    while i <= limit:
        if is_prime[i]:
            count += 1
        i += 1
    return count

print(sieve(50000000))
