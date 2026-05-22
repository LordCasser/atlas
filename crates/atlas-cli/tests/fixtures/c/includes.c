// Golden test fixture: C includes
// Covers: local #include with quotes, function definitions and calls

#include <stdio.h>
#include "helper.h"

int main() {
    int result = helper(42);
    printf("Result: %d\n", result);
    return 0;
}
