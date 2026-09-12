// SEAL arithmetic/serialization microbenchmark. One process holds the full secret key.
// No threshold decryption, valid-input proof, or validator rotation is implemented.
#include <seal/seal.h>
#include <chrono>
#include <sstream>
#include <vector>
#include <iostream>
#include <iomanip>
#include <algorithm>
#include <stdexcept>
using namespace seal;
using Clock=std::chrono::steady_clock;
double ms(Clock::time_point t){return std::chrono::duration<double,std::milli>(Clock::now()-t).count();}
double median(std::vector<double> x){std::sort(x.begin(),x.end());return x[x.size()/2];}
std::streamoff bytes(const Ciphertext &c,compr_mode_type mode){std::ostringstream out(std::ios::binary);return c.save(out,mode);}
void run(size_t n){
 EncryptionParameters parms(scheme_type::bfv);
 parms.set_poly_modulus_degree(n);
 parms.set_coeff_modulus(CoeffModulus::BFVDefault(n,sec_level_type::tc128));
 parms.set_plain_modulus(PlainModulus::Batching(n,50));
 auto start=Clock::now();SEALContext context(parms,true,sec_level_type::tc128);
 if(!context.parameters_set())throw std::runtime_error(context.parameter_error_message());
 KeyGenerator keygen(context);PublicKey pk;keygen.create_public_key(pk);
 auto sk=keygen.secret_key();double setup=ms(start);
 Encryptor encryptor(context,pk);Decryptor decryptor(context,sk);Evaluator evaluator(context);BatchEncoder encoder(context);
 // Sixteen base-2^16 limbs occupy sixteen slots; all other slots are zero.
 // This is ONE independent uint256 contributor per ciphertext, not n clients batched for free.
 std::vector<uint64_t> slots(n,0);
 for(size_t i=0;i<16;i++)slots[i]=65535;
 Plaintext plain;encoder.encode(slots,plain);
 std::vector<double> enc,dec,ser;Ciphertext ct;
 for(int i=0;i<32;i++){auto t=Clock::now();encryptor.encrypt(plain,ct);if(i>=2)enc.push_back(ms(t));}
 auto mode=Serialization::compr_mode_default;
 auto t=Clock::now();auto raw=bytes(ct,compr_mode_type::none);double raw_ser=ms(t);
 std::streamoff packed=0;
 for(int i=0;i<8;i++){auto t=Clock::now();packed=bytes(ct,mode);ser.push_back(ms(t));}
 Ciphertext sum=ct;
 t=Clock::now();for(int i=0;i<4096;i++)evaluator.add_inplace(sum,ct);double add=ms(t)/4096.;
 for(int i=0;i<32;i++){Plaintext out;auto t=Clock::now();decryptor.decrypt(sum,out);std::vector<uint64_t> decoded;encoder.decode(out,decoded);if(i>=2)dec.push_back(ms(t));if(decoded[0]!=65535ULL*4097)throw std::runtime_error("sum mismatch");}
 // Exact 1e9-contribution arithmetic/noise stress by binary addition of the SAME ciphertext.
 uint64_t count=1000000000ULL;Ciphertext power=ct,total;bool initialized=false;
 while(count){if(count&1){if(!initialized){total=power;initialized=true;}else evaluator.add_inplace(total,power);}count>>=1;if(count)evaluator.add_inplace(power,power);}
 Plaintext final_plain;decryptor.decrypt(total,final_plain);std::vector<uint64_t> final_slots;encoder.decode(final_plain,final_slots);
 bool exact=true;for(size_t i=0;i<16;i++)exact=exact&&(final_slots[i]==65535ULL*1000000000ULL);
 size_t qbits=0;for(auto &q:context.first_context_data()->parms().coeff_modulus())qbits+=q.bit_count();
 std::cout<<"{\"scheme\":\"BFV\",\"poly_modulus_degree\":"<<n
 <<",\"data_coeff_modulus_bits\":"<<qbits<<",\"plain_modulus\":"<<parms.plain_modulus().value()
 <<",\"uint256_limbs_per_input\":16,\"limb_bits\":16,\"tc_security_bits\":128"
 <<",\"setup_ms\":"<<setup<<",\"public_key_encrypt_median_ms\":"<<median(enc)
 <<",\"add_one_ciphertext_ms\":"<<add<<",\"decrypt_decode_median_ms\":"<<median(dec)
 <<",\"ciphertext_uncompressed_bytes\":"<<raw<<",\"ciphertext_default_save_bytes\":"<<packed
 <<",\"serialize_uncompressed_ms\":"<<raw_ser<<",\"serialize_default_median_ms\":"<<median(ser)
 <<",\"initial_noise_budget_bits\":"<<decryptor.invariant_noise_budget(ct)
 <<",\"billion_repeat_noise_budget_bits\":"<<decryptor.invariant_noise_budget(total)
 <<",\"billion_repeat_exact\":"<<(exact?"true":"false")
 <<",\"one_process_full_secret_key\":true}";
}
int main(){
 try {std::cout<<std::setprecision(10)<<"{\"version\":\"4.4.0\",\"kind\":\"arithmetic_only_not_threshold_protocol\",\"profiles\":[";
 run(4096);std::cout<<",";run(8192);std::cout<<"]}\n";}
 catch(const std::exception &e){std::cerr<<e.what()<<"\n";return 1;}
}
