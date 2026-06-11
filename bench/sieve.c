#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int sieve(int limit) {
    char *is_prime = malloc(limit + 1);
    memset(is_prime, 1, limit + 1);
    is_prime[0] = is_prime[1] = 0;
    for (int i = 2; i * i <= limit; i++) {
        if (is_prime[i])
            for (int j = i * i; j <= limit; j += i)
                is_prime[j] = 0;
    }
    int count = 0;
    for (int i = 2; i <= limit; i++)
        if (is_prime[i]) count++;
    free(is_prime);
    return count;
}

int main() {
    printf("%d\n", sieve(50000000));
    return 0;
}
